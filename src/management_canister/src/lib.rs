use candid::{decode_one, encode_one, CandidType, Principal};
use ic_cdk::api::caller;
#[cfg(not(test))]
use ic_cdk::api::time;

use serde::{Deserialize, Serialize};
use serde_cbor::Serializer;
use std::fmt;
use std::panic;
use std::{cell::RefCell, collections::HashMap, collections::VecDeque, convert::AsRef};

/// The maximum number of messages returned by [ws_get_messages] at each poll.
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 10;
/// The default delay between two consecutive acknowledgements sent to the client.
const DEFAULT_SEND_ACK_DELAY_MS: u64 = 60_000; // 60 seconds
/// The default delay to wait for the client to send a keep alive after receiving an acknowledgement.
const DEFAULT_CLIENT_KEEP_ALIVE_DELAY_MS: u64 = 10_000; // 10 seconds

pub type ClientPrincipal = Principal;
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug, Hash)]
struct ClientKey {
    client_principal: ClientPrincipal,
    client_nonce: u64,
}

impl ClientKey {
    /// Creates a new instance of ClientKey.
    fn new(client_principal: ClientPrincipal, client_nonce: u64) -> Self {
        Self {
            client_principal,
            client_nonce,
        }
    }
}

impl fmt::Display for ClientKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}_{}", self.client_principal, self.client_nonce)
    }
}

/// The result of [ws_open].
pub type CanisterWsOpenResult = Result<(), String>;
/// The result of [ws_close].
pub type CanisterWsCloseResult = Result<(), String>;
/// The result of [ws_message].
pub type CanisterWsMessageResult = Result<(), String>;
/// The result of [ws_get_messages].
pub type CanisterWsGetMessagesResult = Result<Vec<CanisterOutputMessage>, String>;
/// The result of [ws_send].
pub type CanisterWsSendResult = Result<(), String>;

/// The arguments for [ws_open].
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsOpenArguments {
    client_nonce: u64,
}

/// The arguments for [ws_close].
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsCloseArguments {
    client_key: ClientKey,
}

/// The arguments for [ws_message].
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsMessageArguments {
    msg: WebsocketMessage,
}

/// The arguments for [ws_get_messages].
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsGetMessagesArguments {
    nonce: u64,
}

/// Messages exchanged through the WebSocket.
#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
struct WebsocketMessage {
    client_key: ClientKey, // The client that the gateway will forward the message to or that sent the message.
    sequence_num: u64,     // Both ways, messages should arrive with sequence numbers 0, 1, 2...
    timestamp: u64,        // Timestamp of when the message was made for the recipient to inspect.
    is_service_message: bool, // Whether the message is a service message sent by the CDK to the client or vice versa.
    #[serde(with = "serde_bytes")]
    content: Vec<u8>, // Application message encoded in binary.
}

impl WebsocketMessage {
    /// Serializes the message into a Vec<u8>, using CBOR.
    fn cbor_serialize(&self) -> Result<Vec<u8>, String> {
        let mut data = vec![];
        let mut serializer = Serializer::new(&mut data);
        serializer.self_describe().map_err(|e| e.to_string())?;
        self.serialize(&mut serializer).map_err(|e| e.to_string())?;
        Ok(data)
    }
}

/// Element of the list of messages returned to the WS Gateway after polling.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct CanisterOutputMessage {
    client_key: ClientKey, // The client that the gateway will forward the message to or that sent the message.
    key: String,           // Key for certificate verification.
    #[serde(with = "serde_bytes")]
    content: Vec<u8>, // The message to be relayed, that contains the application message.
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
/// Contains data about the registered WS Gateway.
struct RegisteredGateway {
    /// The principal of the gateway.
    gateway_principal: Principal,
}

impl RegisteredGateway {
    /// Creates a new instance of RegisteredGateway.
    fn new(gateway_principal: Principal) -> Self {
        Self { gateway_principal }
    }
}

fn get_current_time() -> u64 {
    #[cfg(test)]
    {
        0u64
    }
    #[cfg(not(test))]
    {
        time()
    }
}

/// The metadata about a registered client.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RegisteredClient {
    last_keep_alive_timestamp: u64,
}

impl RegisteredClient {
    /// Creates a new instance of RegisteredClient.
    fn new() -> Self {
        Self {
            last_keep_alive_timestamp: get_current_time(),
        }
    }
}

thread_local! {
    /// Maps the client's key to the client metadata
    /* flexible */ static REGISTERED_CLIENTS: RefCell<HashMap<ClientKey, RegisteredClient>> = RefCell::new(HashMap::new());
    /// Maps the client's principal to the current client key
    /* flexible */ static CURRENT_CLIENT_KEY_MAP: RefCell<HashMap<ClientPrincipal, ClientKey>> = RefCell::new(HashMap::new());
    /// Maps the client's key to the sequence number to use for the next outgoing message (to that client).
    /* flexible */ static OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP: RefCell<HashMap<ClientKey, u64>> = RefCell::new(HashMap::new());
    /// Maps the client's key to the expected sequence number of the next incoming message (from that client).
    /* flexible */ static INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP: RefCell<HashMap<ClientKey, u64>> = RefCell::new(HashMap::new());
    /// Keeps track of the principal of the WS Gateway which polls the canister
    /* flexible */ static REGISTERED_GATEWAY: RefCell<Option<RegisteredGateway>> = RefCell::new(None);
    /// Keeps track of the messages that have to be sent to the WS Gateway
    /* flexible */ static MESSAGES_FOR_GATEWAY: RefCell<VecDeque<CanisterOutputMessage>> = RefCell::new(VecDeque::new());
    /// Keeps track of the nonce which:
    /// - the WS Gateway uses to specify the first index of the certified messages to be returned when polling
    /// - the client uses as part of the path in the Merkle tree in order to verify the certificate of the messages relayed by the WS Gateway
    /* flexible */ static OUTGOING_MESSAGE_NONCE: RefCell<u64> = RefCell::new(0u64);
    /// The callback handlers for the WebSocket.
    /* flexible */ static HANDLERS: RefCell<WsHandlers> = RefCell::new(WsHandlers {
        on_open: None,
        on_message: None,
        on_close: None,
    });
}

/// Resets all RefCells to their initial state.
fn reset_internal_state() {
    let client_keys_to_remove: Vec<ClientKey> = REGISTERED_CLIENTS.with(|state| {
        let map = state.borrow();
        map.keys().cloned().collect()
    });

    // for each client, call the on_close handler before clearing the map
    for client_key in client_keys_to_remove {
        remove_client(&client_key);
    }

    CURRENT_CLIENT_KEY_MAP.with(|map| {
        map.borrow_mut().clear();
    });

    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    MESSAGES_FOR_GATEWAY.with(|m| *m.borrow_mut() = VecDeque::new());
    OUTGOING_MESSAGE_NONCE.with(|next_id| next_id.replace(0u64));
}

/// Resets the internal state of the IC WebSocket CDK.
///
/// **Note:** You should only call this function in tests.
pub fn wipe() {
    reset_internal_state();
}

fn get_outgoing_message_nonce() -> u64 {
    OUTGOING_MESSAGE_NONCE.with(|n| n.borrow().clone())
}

fn increment_outgoing_message_nonce() {
    OUTGOING_MESSAGE_NONCE.with(|n| n.replace_with(|&mut old| old + 1));
}

fn insert_client(client_key: ClientKey, new_client: RegisteredClient) {
    CURRENT_CLIENT_KEY_MAP.with(|map| {
        map.borrow_mut()
            .insert(client_key.client_principal.clone(), client_key.clone());
    });
    REGISTERED_CLIENTS.with(|map| {
        map.borrow_mut().insert(client_key, new_client);
    });
}

fn is_client_registered(client_key: &ClientKey) -> bool {
    REGISTERED_CLIENTS.with(|map| map.borrow().contains_key(client_key))
}

fn get_client_key_from_principal(client_principal: &ClientPrincipal) -> Result<ClientKey, String> {
    CURRENT_CLIENT_KEY_MAP.with(|map| {
        map.borrow()
            .get(client_principal)
            .cloned()
            .ok_or(String::from(format!(
                "client with principal {} doesn't have an open connection",
                client_principal
            )))
    })
}

fn check_registered_client(client_key: &ClientKey) -> Result<(), String> {
    if !is_client_registered(client_key) {
        return Err(String::from(format!(
            "client with key {} doesn't have an open connection",
            client_key
        )));
    }

    Ok(())
}

fn initialize_registered_gateway(gateway_principal: &str) {
    REGISTERED_GATEWAY.with(|p| {
        let gateway_principal =
            Principal::from_text(gateway_principal).expect("invalid gateway principal");
        *p.borrow_mut() = Some(RegisteredGateway::new(gateway_principal));
    });
}

fn get_registered_gateway_principal() -> Principal {
    REGISTERED_GATEWAY.with(|g| {
        g.borrow()
            .expect("gateway should be initialized")
            .gateway_principal
    })
}

fn init_outgoing_message_to_client_num(client_key: ClientKey) {
    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().insert(client_key, 0);
    });
}

fn get_outgoing_message_to_client_num(client_key: &ClientKey) -> Result<u64, String> {
    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        let map = map.borrow();
        let num = *map.get(client_key).ok_or(String::from(
            "outgoing message to client num not initialized for client",
        ))?;
        Ok(num)
    })
}

fn increment_outgoing_message_to_client_num(client_key: &ClientKey) -> Result<(), String> {
    let num = get_outgoing_message_to_client_num(client_key)?;
    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        let mut map = map.borrow_mut();
        map.insert(client_key.clone(), num + 1);
        Ok(())
    })
}

fn init_expected_incoming_message_from_client_num(client_key: ClientKey) {
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().insert(client_key, 1);
    });
}

fn get_expected_incoming_message_from_client_num(client_key: &ClientKey) -> Result<u64, String> {
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        let num = *map.borrow().get(client_key).ok_or(String::from(
            "expected incoming message num not initialized for client",
        ))?;
        Ok(num)
    })
}

fn increment_expected_incoming_message_from_client_num(
    client_key: &ClientKey,
) -> Result<(), String> {
    let num = get_expected_incoming_message_from_client_num(client_key)?;
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        let mut map = map.borrow_mut();
        map.insert(client_key.clone(), num + 1);
        Ok(())
    })
}

fn add_client(client_key: ClientKey, new_client: RegisteredClient) {
    // insert the client in the map
    insert_client(client_key.clone(), new_client);
    // initialize incoming client's message sequence number to 1
    init_expected_incoming_message_from_client_num(client_key.clone());
    // initialize outgoing message sequence number to 0
    init_outgoing_message_to_client_num(client_key);
}

fn remove_client(client_key: &ClientKey) {
    CURRENT_CLIENT_KEY_MAP.with(|map| {
        map.borrow_mut().remove(&client_key.client_principal);
    });
    REGISTERED_CLIENTS.with(|map| {
        map.borrow_mut().remove(client_key);
    });
    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().remove(client_key);
    });
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().remove(client_key);
    });

    let handlers = HANDLERS.with(|state| state.borrow().clone());
    handlers.call_on_close(OnCloseCallbackArgs {
        client_principal: client_key.client_principal,
    });
}

fn get_message_for_gateway_key(gateway_principal: Principal, nonce: u64) -> String {
    gateway_principal.to_string() + "_" + &format!("{:0>20}", nonce.to_string())
}

fn get_messages_for_gateway_range(gateway_principal: Principal, nonce: u64) -> (usize, usize) {
    MESSAGES_FOR_GATEWAY.with(|m| {
        let queue_len = m.borrow().len();

        if nonce == 0 && queue_len > 0 {
            // this is the case in which the poller on the gateway restarted
            // the range to return is end:last index and start: max(end - MAX_NUMBER_OF_RETURNED_MESSAGES, 0)
            let start_index = if queue_len > MAX_NUMBER_OF_RETURNED_MESSAGES {
                queue_len - MAX_NUMBER_OF_RETURNED_MESSAGES
            } else {
                0
            };

            return (start_index, queue_len);
        }

        // smallest key used to determine the first message from the queue which has to be returned to the WS Gateway
        let smallest_key = get_message_for_gateway_key(gateway_principal, nonce);
        // partition the queue at the message which has the key with the nonce specified as argument to get_cert_messages
        let start_index = m.borrow().partition_point(|x| x.key < smallest_key);
        // message at index corresponding to end index is excluded
        let mut end_index = queue_len;
        if end_index - start_index > MAX_NUMBER_OF_RETURNED_MESSAGES {
            end_index = start_index + MAX_NUMBER_OF_RETURNED_MESSAGES;
        }
        (start_index, end_index)
    })
}

fn get_messages_for_gateway(start_index: usize, end_index: usize) -> Vec<CanisterOutputMessage> {
    MESSAGES_FOR_GATEWAY.with(|m| {
        let mut messages: Vec<CanisterOutputMessage> = Vec::with_capacity(end_index - start_index);
        for index in start_index..end_index {
            messages.push(m.borrow().get(index).unwrap().clone());
        }
        messages
    })
}

/// Gets the messages in MESSAGES_FOR_GATEWAY starting from the one with the specified nonce
fn get_cert_messages(gateway_principal: Principal, nonce: u64) -> CanisterWsGetMessagesResult {
    let (start_index, end_index) = get_messages_for_gateway_range(gateway_principal, nonce);
    Ok(get_messages_for_gateway(start_index, end_index))
}

fn is_registered_gateway(principal: Principal) -> bool {
    let registered_gateway_principal = get_registered_gateway_principal();
    return registered_gateway_principal == principal;
}

/// Checks if the caller of the method is the same as the one that was registered during the initialization of the CDK
fn check_is_registered_gateway(input_principal: Principal) -> Result<(), String> {
    let gateway_principal = get_registered_gateway_principal();
    // check if the caller is the same as the one that was registered during the initialization of the CDK
    if gateway_principal != input_principal {
        return Err(String::from(
            "caller is not the gateway that has been registered during CDK initialization",
        ));
    }
    Ok(())
}

#[derive(CandidType, Deserialize)]
struct CanisterOpenMessageContent {
    client_key: ClientKey,
}

#[derive(CandidType, Deserialize)]
struct CanisterAckMessageContent {
    last_incoming_sequence_num: u64,
}

#[derive(CandidType, Deserialize)]
struct ClientKeepAliveMessageContent {
    last_incoming_sequence_num: u64,
}

/// A service message sent by the CDK to the client or vice versa.
#[derive(CandidType, Deserialize)]
enum WebsocketServiceMessageContent {
    /// Message sent by the **canister** when a client opens a connection.
    OpenMessage(CanisterOpenMessageContent),
    /// Message sent _periodically_ by the **canister** to the client to acknowledge the messages received.
    AckMessage(CanisterAckMessageContent),
    /// Message sent by the **client** in response to an acknowledgement message from the canister.
    KeepAliveMessage(ClientKeepAliveMessageContent),
}

impl WebsocketServiceMessageContent {
    fn from_candid_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        decode_one(&bytes).map_err(|e| {
            let mut err = String::from("Error decoding service message content: ");
            err.push_str(&e.to_string());
            err
        })
    }
}

fn send_service_message_to_client(
    client_key: &ClientKey,
    message: WebsocketServiceMessageContent,
) -> Result<(), String> {
    let message_bytes = encode_one(&message).unwrap();
    _ws_send(client_key, message_bytes, true)
}

/// Internal function used to put the messages in the outgoing messages queue and certify them.
fn _ws_send(
    client_key: &ClientKey,
    msg_bytes: Vec<u8>,
    is_service_message: bool,
) -> CanisterWsSendResult {
    // check if the client is registered
    check_registered_client(client_key)?;

    // get the principal of the gateway that is polling the canister
    let gateway_principal = get_registered_gateway_principal();

    // the nonce in key is used by the WS Gateway to determine the message to start in the polling iteration
    // the key is also passed to the client in order to validate the body of the certified message
    let outgoing_message_nonce = get_outgoing_message_nonce();
    let key = get_message_for_gateway_key(gateway_principal, outgoing_message_nonce);

    // increment the nonce for the next message
    increment_outgoing_message_nonce();
    // increment the sequence number for the next message to the client
    increment_outgoing_message_to_client_num(client_key)?;

    let websocket_message = WebsocketMessage {
        client_key: client_key.clone(),
        sequence_num: get_outgoing_message_to_client_num(client_key)?,
        timestamp: get_current_time(),
        is_service_message,
        content: msg_bytes,
    };

    // CBOR serialize message of type WebsocketMessage
    let content = websocket_message.cbor_serialize()?;

    MESSAGES_FOR_GATEWAY.with(|m| {
        // messages in the queue are inserted with contiguous and increasing nonces
        // (from beginning to end of the queue) as ws_send is called sequentially, the nonce
        // is incremented by one in each call, and the message is pushed at the end of the queue
        m.borrow_mut().push_back(CanisterOutputMessage {
            client_key: client_key.clone(),
            content,
            key,
        });
    });
    Ok(())
}

fn handle_received_service_message(content: Vec<u8>) -> CanisterWsMessageResult {
    let decoded = WebsocketServiceMessageContent::from_candid_bytes(content)?;
    match decoded {
        WebsocketServiceMessageContent::OpenMessage(_)
        | WebsocketServiceMessageContent::AckMessage(_) => {
            Err(String::from("Invalid received service message"))
        },
        WebsocketServiceMessageContent::KeepAliveMessage(_) => {
            print!("Service message handling not implemented yet");
            Ok(())
        },
    }
}

/// Arguments passed to the `on_open` handler.
pub struct OnOpenCallbackArgs {
    pub client_principal: ClientPrincipal,
}
/// Handler initialized by the canister
/// and triggered by the CDK once the IC WebSocket connection is established.
type OnOpenCallback = fn(OnOpenCallbackArgs);

/// Arguments passed to the `on_message` handler.
pub struct OnMessageCallbackArgs {
    pub client_principal: ClientPrincipal,
    pub message: Vec<u8>,
}
/// Handler initialized by the canister
/// and triggered by the CDK once an IC WebSocket message is received.
type OnMessageCallback = fn(OnMessageCallbackArgs);

/// Arguments passed to the `on_close` handler.
pub struct OnCloseCallbackArgs {
    pub client_principal: ClientPrincipal,
}
/// Handler initialized by the canister
/// and triggered by the CDK once the WS Gateway closes the IC WebSocket connection
/// for that client.
type OnCloseCallback = fn(OnCloseCallbackArgs);

/// Handlers initialized by the canister and triggered by the CDK.
#[derive(Clone, Default)]
pub struct WsHandlers {
    pub on_open: Option<OnOpenCallback>,
    pub on_message: Option<OnMessageCallback>,
    pub on_close: Option<OnCloseCallback>,
}

impl WsHandlers {
    fn call_on_open(&self, args: OnOpenCallbackArgs) {
        if let Some(on_open) = self.on_open {
            let res = panic::catch_unwind(|| {
                on_open(args);
            });

            if let Err(e) = res {
                print!("Error calling on_open handler: {:?}", e);
            }
        }
    }

    fn call_on_message(&self, args: OnMessageCallbackArgs) {
        if let Some(on_message) = self.on_message {
            let res = panic::catch_unwind(|| {
                on_message(args);
            });

            if let Err(e) = res {
                print!("Error calling on_message handler: {:?}", e);
            }
        }
    }

    fn call_on_close(&self, args: OnCloseCallbackArgs) {
        if let Some(on_close) = self.on_close {
            let res = panic::catch_unwind(|| {
                on_close(args);
            });

            if let Err(e) = res {
                print!("Error calling on_close handler: {:?}", e);
            }
        }
    }
}

fn initialize_handlers(handlers: WsHandlers) {
    HANDLERS.with(|h| {
        let mut h = h.borrow_mut();
        *h = handlers;
    });
}

/// Parameters for the IC WebSocket CDK initialization. For default parameters and simpler initialization, use [`WsInitParams::new`].
#[derive(Clone)]
pub struct WsInitParams {
    /// The callback handlers for the WebSocket.
    pub handlers: WsHandlers,
    /// The principal of the WS Gateway that will be polling the canister.
    pub gateway_principal: String,
    /// The interval at which to send an acknowledgement message to the client,
    /// so that the client knows that all the messages it sent have been received by the canister (in milliseconds).
    /// Defaults to `60_000` (60 seconds).
    pub send_ack_interval_ms: u64,
    /// The delay to wait for the client to send a keep alive after receiving an acknowledgement (in milliseconds).
    /// Defaults to `10_000` (10 seconds).
    pub keep_alive_delay_ms: u64,
}

impl WsInitParams {
    /// Creates a new instance of WsInitParams, with default interval values.
    pub fn new(handlers: WsHandlers, gateway_principal: String) -> Self {
        Self {
            handlers,
            gateway_principal,
            send_ack_interval_ms: DEFAULT_SEND_ACK_DELAY_MS,
            keep_alive_delay_ms: DEFAULT_CLIENT_KEEP_ALIVE_DELAY_MS,
        }
    }
}

/// Initialize the CDK by setting the callback handlers and the **principal** of the WS Gateway that
/// will be polling the canister.
///
/// Under the hood, an interval (**60 seconds**) is started using [ic_cdk_timers::set_timer]
/// to check if the WS Gateway is still alive.
pub fn init(params: WsInitParams) {
    // set the handlers specified by the canister that the CDK uses to manage the IC WebSocket connection
    initialize_handlers(params.handlers);

    // set the principal of the (only) WS Gateway that will be polling the canister
    initialize_registered_gateway(&params.gateway_principal);
}

/// Handles the WS connection open event sent by the client and relayed by the Gateway.
pub fn ws_open(args: CanisterWsOpenArguments) -> CanisterWsOpenResult {
    let client_principal = caller();
    // anonymous clients cannot open a connection
    if client_principal == ClientPrincipal::anonymous() {
        return Err(String::from("anonymous principal cannot open a connection"));
    }

    // avoid gateway opening a connection for its own principal
    if is_registered_gateway(client_principal) {
        return Err(String::from(
            "caller is the registered gateway which can't open a connection for itself",
        ));
    }

    let client_key = ClientKey::new(client_principal, args.client_nonce);
    // check if client is not registered yet
    if is_client_registered(&client_key) {
        return Err(format!(
            "client with key {} already has an open connection",
            client_key,
        ));
    }

    // initialize client maps
    let new_client = RegisteredClient::new();
    add_client(client_key.clone(), new_client);

    let open_message = CanisterOpenMessageContent {
        client_key: client_key.clone(),
    };
    let message = WebsocketServiceMessageContent::OpenMessage(open_message);
    send_service_message_to_client(&client_key, message)?;

    // call the on_open handler initialized in init()
    HANDLERS.with(|h| {
        h.borrow()
            .call_on_open(OnOpenCallbackArgs { client_principal });
    });

    Ok(())
}

/// Handles the WS connection close event received from the WS Gateway.
pub fn ws_close(args: CanisterWsCloseArguments) -> CanisterWsCloseResult {
    // the caller must be the gateway that was registered during CDK initialization
    check_is_registered_gateway(caller())?;

    // check if client registered its principal by calling ws_open
    check_registered_client(&args.client_key)?;

    remove_client(&args.client_key);

    Ok(())
}

/// Handles the WS messages received either directly from the client or relayed by the WS Gateway.
pub fn ws_message(args: CanisterWsMessageArguments) -> CanisterWsMessageResult {
    let client_principal = caller();
    // check if client registered its principal by calling ws_open
    let registered_client_key = get_client_key_from_principal(&client_principal)?;

    let WebsocketMessage {
        client_key,
        sequence_num,
        timestamp: _,
        is_service_message,
        content,
    } = args.msg;

    // check if the client key is correct
    if registered_client_key != client_key {
        return Err(String::from(format!(
            "client with principal {} has a different key than the one used in the message",
            client_principal
        )));
    }

    let expected_sequence_num = get_expected_incoming_message_from_client_num(&client_key)?;

    // check if the incoming message has the expected sequence number
    if sequence_num != expected_sequence_num {
        remove_client(&client_key);
        return Err(String::from(
            format!(
                "incoming client's message does not have the expected sequence number. Expected: {expected_sequence_num}, actual: {sequence_num}. Client removed.",
            ),
        ));
    }
    // increase the expected sequence number by 1
    increment_expected_incoming_message_from_client_num(&client_key)?;

    if is_service_message {
        return handle_received_service_message(content);
    }

    // call the on_message handler initialized in init()
    HANDLERS.with(|h| {
        // trigger the on_message handler initialized by canister
        // create message to send to client
        h.borrow().call_on_message(OnMessageCallbackArgs {
            client_principal,
            message: content,
        });
    });
    Ok(())
}

/// Returns messages to the WS Gateway in response of a polling iteration.
pub fn ws_get_messages(args: CanisterWsGetMessagesArguments) -> CanisterWsGetMessagesResult {
    // check if the caller of this method is the WS Gateway that has been set during the initialization of the SDK
    let gateway_principal = caller();
    check_is_registered_gateway(gateway_principal)?;

    get_cert_messages(gateway_principal, args.nonce)
}

/// Sends a message to the client. The message must already be serialized, using a method of your choice, like Candid or CBOR.
///
/// Under the hood, the message is serialized and certified, and then it is added to the queue of messages
/// that the WS Gateway will poll in the next iteration.
pub fn ws_send(client_principal: ClientPrincipal, msg_bytes: Vec<u8>) -> CanisterWsSendResult {
    let client_key = get_client_key_from_principal(&client_principal)?;
    _ws_send(&client_key, msg_bytes, false)
}

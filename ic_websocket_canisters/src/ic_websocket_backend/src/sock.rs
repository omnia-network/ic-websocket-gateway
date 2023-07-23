use candid::{CandidType, Principal};
use ed25519_compact::{PublicKey, Signature};
use ic_cdk::api::{caller, data_certificate, set_certified_data, time};
use ic_cdk::print;
use ic_certified_map::{labeled, labeled_hash, AsHashTree, Hash as ICHash, RbTree};
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, Serializer};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, collections::HashMap, collections::VecDeque, convert::AsRef};

const LABEL_WEBSOCKET: &[u8] = b"websocket";
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 10;

pub type ClientPublicKey = Vec<u8>;

/// The result of `ws_register`.
pub type CanisterWsRegisterResult = Result<(), String>;
/// The result of `ws_open`.
pub type CanisterWsOpenResult = Result<CanisterWsOpenResultValue, String>;
/// The result of `ws_message`.
pub type CanisterWsMessageResult = Result<(), String>;
/// The result of `ws_get_messages`.
pub type CanisterWsGetMessagesResult = Result<CanisterOutputCertifiedMessages, String>;
/// The result of `ws_send`.
pub type CanisterWsSendResult = Result<(), String>;
/// The result of `ws_close`.
pub type CanisterWsCloseResult = Result<(), String>;

// The Ok value of CanisterWsOpenResult returned by `ws_open`
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsOpenResultValue {
    client_key: ClientPublicKey,
    canister_id: Principal,
    nonce: u64,
}

/// The arguments for `ws_register`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
pub struct CanisterWsRegisterArguments {
    #[serde(with = "serde_bytes")]
    client_key: ClientPublicKey,
}

/// The arguments for `ws_open`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
pub struct CanisterWsOpenArguments {
    #[serde(with = "serde_bytes")]
    msg: Vec<u8>,
    #[serde(with = "serde_bytes")]
    sig: Vec<u8>,
}

/// The arguments for `ws_close`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
pub struct CanisterWsCloseArguments {
    #[serde(with = "serde_bytes")]
    client_key: ClientPublicKey,
}

/// The arguments for `ws_message`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
pub struct CanisterWsMessageArguments {
    msg: CanisterIncomingMessage,
}

/// The arguments for `ws_get_messages`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
pub struct CanisterWsGetMessagesArguments {
    nonce: u64,
}

/// The first message received by the canister in `ws_open`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
struct CanisterFirstMessageContent {
    #[serde(with = "serde_bytes")]
    client_key: ClientPublicKey,
    canister_id: Principal,
}

/// Message + signature from client, **relayed** by the WS Gateway.
#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct RelayedClientMessage {
    #[serde(with = "serde_bytes")]
    content: Vec<u8>,
    #[serde(with = "serde_bytes")]
    sig: Vec<u8>,
}

/// Message coming directly from client, not relayed by the WS Gateway.
#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct DirectClientMessage {
    pub message: Vec<u8>,
    pub client_key: ClientPublicKey,
}

/// The variants of the possible messages received by the canister in `ws_message`.
/// - IcWebSocketEstablished: message sent from WS Gateway to the canister to notify it about the
///                           establishment of the IcWebSocketConnection
/// - RelayedByGateway: message sent from the client to the WS Gateway (via WebSocket) and
///                      relayed to the canister by the WS Gateway
/// - DirectlyFromClient: message sent from directly client so that it is not necessary to
///                       verify the signature
#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub enum CanisterIncomingMessage {
    DirectlyFromClient(DirectClientMessage),
    RelayedByGateway(RelayedClientMessage),
    IcWebSocketEstablished(ClientPublicKey),
}

/// Messages exchanged through the WebSocket.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct WebsocketMessage {
    #[serde(with = "serde_bytes")]
    pub client_key: ClientPublicKey, // To or from client key.
    pub sequence_num: u64, // Both ways, messages should arrive with sequence numbers 0, 1, 2...
    pub timestamp: u64,    // Timestamp of when the message was made for the recipient to inspect.
    #[serde(with = "serde_bytes")]
    pub message: Vec<u8>, // Application message encoded in binary.
}

/// Element of the list of messages returned to the WS Gateway after polling.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct CanisterOutputMessage {
    #[serde(with = "serde_bytes")]
    client_key: ClientPublicKey, // The client that the gateway will forward the message to.
    key: String, // Key for certificate verification.
    #[serde(with = "serde_bytes")]
    val: Vec<u8>, // Encoded WebsocketMessage.
}

/// List of messages returned to the WS Gateway after polling.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct CanisterOutputCertifiedMessages {
    messages: Vec<CanisterOutputMessage>, // List of messages.
    #[serde(with = "serde_bytes")]
    cert: Vec<u8>, // cert+tree constitute the certificate for all returned messages.
    #[serde(with = "serde_bytes")]
    tree: Vec<u8>, // cert+tree constitute the certificate for all returned messages.
}

thread_local! {
    /// Maps the client's public key to the client's identity (anonymous if not authenticated).
    static CLIENT_CALLER_MAP: RefCell<HashMap<ClientPublicKey, Principal>> = RefCell::new(HashMap::new());
    /// Maps the client's public key to the sequence number to use for the next outgoing message (to that client).
    static OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP: RefCell<HashMap<ClientPublicKey, u64>> = RefCell::new(HashMap::new());
    /// Maps the client's public key to the expected sequence number of the next incoming message (from that client).
    static INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP: RefCell<HashMap<ClientPublicKey, u64>> = RefCell::new(HashMap::new());
    /// Keeps track of the Merkle tree used for certified queries
    static CERT_TREE: RefCell<RbTree<String, ICHash>> = RefCell::new(RbTree::new());
    /// Keeps track of the principal of the WS Gateway which polls the canister
    static GATEWAY_PRINCIPAL: RefCell<Option<Principal>> = RefCell::new(None);
    /// Keeps tarck of the messages that have to be sent to the WS Gateway
    static MESSAGES_FOR_GATEWAY: RefCell<VecDeque<CanisterOutputMessage>> = RefCell::new(VecDeque::new());
    /// Keeps track of the nonce which:
    /// - the WS Gateway uses to specify the first index of the certified messages to be returned when polling
    /// - the client uses as part of the path in the Merkle tree in order to verify the certificate of the messages relayed by the WS Gateway
    static OUTGOING_MESSAGE_NONCE: RefCell<u64> = RefCell::new(0u64);
}

pub fn wipe() {
    CLIENT_CALLER_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    CERT_TREE.with(|t| {
        t.replace(RbTree::new());
    });
    GATEWAY_PRINCIPAL.with(|p| {
        *p.borrow_mut() = None;
    });
    MESSAGES_FOR_GATEWAY.with(|m| *m.borrow_mut() = VecDeque::new());
    OUTGOING_MESSAGE_NONCE.with(|next_id| next_id.replace(0u64));
}

fn get_outgoing_message_nonce() -> u64 {
    OUTGOING_MESSAGE_NONCE.with(|n| n.borrow().clone())
}

fn next_outgoing_message_nonce() -> u64 {
    OUTGOING_MESSAGE_NONCE.with(|n| n.replace_with(|&mut old| old + 1))
}

fn put_client_caller(client_key: ClientPublicKey) {
    CLIENT_CALLER_MAP.with(|map| {
        map.borrow_mut().insert(client_key, caller());
    })
}

fn get_client_caller(client_key: &ClientPublicKey) -> Option<Principal> {
    CLIENT_CALLER_MAP.with(|map| Some(map.borrow().get(client_key)?.to_owned()))
}

fn get_gateway_principal() -> Principal {
    GATEWAY_PRINCIPAL.with(|g| g.borrow().expect("gateway principal should be initialized"))
}

fn check_registered_client_key(client_key: &ClientPublicKey) -> Result<(), String> {
    match CLIENT_CALLER_MAP.with(|map| map.borrow().contains_key(client_key)) {
        true => Ok(()),
        false => Err(String::from(
            "client's public key has not been previously registered by client",
        )),
    }
}

fn init_outgoing_message_to_client_num(client_key: ClientPublicKey) {
    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().insert(client_key, 0);
    });
}

fn next_outgoing_message_to_client_num(client_key: &ClientPublicKey) -> Result<u64, String> {
    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        let mut map = map.borrow_mut();
        let num = *map
            .get(client_key)
            .ok_or(String::from("next message num not initialized for client"))?;
        map.insert(client_key.clone(), num + 1);
        Ok(num + 1)
    })
}

fn init_expected_incoming_message_from_client_num(client_key: ClientPublicKey) {
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().insert(client_key, 0);
    });
}

fn get_expected_incoming_message_from_client_num(
    client_key: &ClientPublicKey,
) -> Result<u64, String> {
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        let num = *map.borrow().get(client_key).ok_or(String::from(
            "incoming message num not initialized for client",
        ))?;
        Ok(num)
    })
}

fn increment_expected_incoming_message_from_client_num(
    client_key: &ClientPublicKey,
) -> Result<u64, String> {
    let num = get_expected_incoming_message_from_client_num(client_key)?;
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP
        .with(|map| map.borrow_mut().insert(client_key.clone(), num + 1));
    Ok(num + 1)
}

fn add_client(client_key: ClientPublicKey) {
    // initialize incoming client's message sequence number to 0
    init_expected_incoming_message_from_client_num(client_key.clone());
    // initialize outgoing message sequence number to 0
    init_outgoing_message_to_client_num(client_key);
}

fn remove_client(client_key: ClientPublicKey) {
    CLIENT_CALLER_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
}

// gets the messages in GATEWAY_MESSAGES starting from the one with the specified nonce
fn get_cert_messages(nonce: u64) -> CanisterWsGetMessagesResult {
    let gateway_principal = get_gateway_principal();

    MESSAGES_FOR_GATEWAY.with(|m| {
        // smallest key  used to determine the first of the messages from the queue which has to be returned to the WS Gateway
        let smallest_key =
            gateway_principal.to_string() + "_" + &format!("{:0>20}", nonce.to_string());
        // partition the queue at the message which has the key with the nonce specified as argument to get_cert_messages
        let start_index = m.borrow().partition_point(|x| x.key < smallest_key);
        // message at index corresponding to end index is excluded
        let end_index;
        if m.borrow().len() - start_index > MAX_NUMBER_OF_RETURNED_MESSAGES {
            end_index = start_index + MAX_NUMBER_OF_RETURNED_MESSAGES;
        } else {
            end_index = m.borrow().len();
        }
        let mut messages: Vec<CanisterOutputMessage> = Vec::with_capacity(end_index - start_index);
        for index in start_index..end_index {
            messages.push(m.borrow().get(index).unwrap().clone());
        }
        if end_index > start_index {
            let first_key = messages.first().unwrap().key.clone();
            let last_key = messages.last().unwrap().key.clone();
            let (cert, tree) = get_cert_for_range(&first_key, &last_key);
            Ok(CanisterOutputCertifiedMessages {
                messages,
                cert,
                tree,
            })
        } else {
            Ok(CanisterOutputCertifiedMessages {
                messages,
                cert: Vec::new(),
                tree: Vec::new(),
            })
        }
    })
}

/// Checks if the caller of the method is the same as the one that was registered during the initialization of the CDK
fn check_caller_is_registered_gateway() -> Result<(), String> {
    let gateway_principal = get_gateway_principal();
    // check if the caller is the same as the one that was registered during the initialization of the CDK
    if gateway_principal != caller() {
        return Err(String::from(
            "caller is not the gateway that has been registered during CDK initialization",
        ));
    }
    Ok(())
}

fn put_cert_for_message(key: String, value: &Vec<u8>) {
    let root_hash = CERT_TREE.with(|tree| {
        let mut tree = tree.borrow_mut();
        tree.insert(key.clone(), Sha256::digest(value).into());
        labeled_hash(LABEL_WEBSOCKET, &tree.root_hash())
    });

    set_certified_data(&root_hash);
}

fn get_cert_for_range(first: &String, last: &String) -> (Vec<u8>, Vec<u8>) {
    CERT_TREE.with(|tree| {
        let tree = tree.borrow();
        let witness = tree.value_range(first.as_ref(), last.as_ref());
        let tree = labeled(LABEL_WEBSOCKET, witness);

        let mut data = vec![];
        let mut serializer = Serializer::new(&mut data);
        serializer.self_describe().unwrap();
        tree.serialize(&mut serializer).unwrap();
        (data_certificate().unwrap(), data)
    })
}

// type ApplicationMessage<T: Deserialize + Serialize> = T;

/// handler initialized by the canister and triggered by the CDK once the IC WebSocket connection
/// is established
type OnOpenCallback = fn(ClientPublicKey);
/// handler initialized by the canister and triggered by the CDK once a message is received by
/// the CDk (either directly from the client or relayed by the WS Gateway)
type OnMessageCallback = fn(DirectClientMessage);
/// handler initialized by the canister and triggered by the CDK once the WS Gateway closes the
/// IC WebSocket connection
type OnCloseCallback = fn(ClientPublicKey);

struct WsHandlers {
    on_open: Result<OnOpenCallback, String>,
    on_message: Result<OnMessageCallback, String>,
    on_close: Result<OnCloseCallback, String>,
}

thread_local! {
    /* flexible */ static HANDLERS: RefCell<WsHandlers> = RefCell::new(WsHandlers {
        on_open: Err(String::from("on_open handler not initialized")),
        on_message: Err(String::from("on_message handler not initialized")),
        on_close: Err(String::from("on_close handler not initialized")),
    });
}

// init CDK
pub fn init(
    on_open: OnOpenCallback,
    on_message: OnMessageCallback,
    on_close: OnCloseCallback,
    gateway_principal: &str,
) {
    // set the handlers specified by the canister that the CDK uses to manage the IC WebSocket connection
    HANDLERS.with(|h| {
        let mut h = h.borrow_mut();
        h.on_open = Ok(on_open);
        h.on_message = Ok(on_message);
        h.on_close = Ok(on_close);
    });
    // set the principal of the (only) WS Gateway that will be polling the canister
    GATEWAY_PRINCIPAL.with(|p| {
        let gateway_principal =
            Principal::from_text(gateway_principal).expect("invalid gateway principal");
        *p.borrow_mut() = Some(gateway_principal);
    });
}

/// Handles the register event received from the client.
///
/// Registers the public key that the client SDK has generated to initialize an IcWebSocket connection.
pub fn ws_register(args: CanisterWsRegisterArguments) -> CanisterWsRegisterResult {
    // TODO: check who is the caller, which can be a client or the anonymous principal
    // associate the identity of the client to its public key received as input
    put_client_caller(args.client_key);
    Ok(())
}

/// Handles the WS connection open event received from the WS Gateway
///
/// WS Gateway relays the first message sent by the client together with its signature
/// to prove that the first message is actually coming from the same client that registered its public key
/// beforehand by calling ws_register()
pub fn ws_open(args: CanisterWsOpenArguments) -> CanisterWsOpenResult {
    // the caller must be the gateway that was registered during CDK initialization
    check_caller_is_registered_gateway()?;
    // decode the first message sent by the client
    let CanisterFirstMessageContent {
        client_key,
        canister_id,
    } = from_slice(&args.msg).map_err(|e| e.to_string())?;
    // check if client_key is a Ed25519 public key
    let public_key = PublicKey::from_slice(&client_key).map_err(|e| e.to_string())?;
    // check if the signature realyed by the WS Gateway is a Ed25519 signature
    let sig = Signature::from_slice(&args.sig).map_err(|e| e.to_string())?;

    // check if client registered its public key by calling ws_register
    check_registered_client_key(&client_key)?;
    // check if the signature on the first message verifies against the public key of the registered client
    // if so, the first message came from the same client that registered its public key using ws_register
    public_key
        .verify(&args.msg, &sig)
        .map_err(|e| e.to_string())?;

    // initialize client maps
    add_client(client_key.clone());

    Ok(CanisterWsOpenResultValue {
        client_key,
        canister_id,
        // returns the current nonce so that in case the WS Gateway has to open a new poller for this canister
        // it knows which nonce to start polling from. This is needed in order to make sure that the WS Gateway
        // does not poll messages it has already relayed when a new it starts polling a canister
        // (which it might have already polled previously with another thread that was closed after the last client disconnected)
        nonce: get_outgoing_message_nonce(),
    })
}

/// Handles the WS connection close event received from the WS Gateway
pub fn ws_close(args: CanisterWsCloseArguments) -> CanisterWsCloseResult {
    // the caller must be the gateway that was registered during CDK initialization
    check_caller_is_registered_gateway()?;

    remove_client(args.client_key.clone());

    HANDLERS.with(|h| {
        let on_close_handler = h.borrow().on_close.to_owned()?;
        on_close_handler(args.client_key);
        Ok(())
    })
}

/// Handles the WS messages received  either directly from the client or relayed by the WS Gateway
pub fn ws_message(args: CanisterWsMessageArguments) -> CanisterWsMessageResult {
    // TODO: check the caller, which can be either the WS Gateway or the client, but not another canister or the anonymous principal
    match args.msg {
        // message sent directly from client
        CanisterIncomingMessage::DirectlyFromClient(canister_message) => {
            // check if the identity of the caller corresponds to the one registered for the given public key
            if caller()
                != get_client_caller(&canister_message.client_key).ok_or(String::from(
                    "client was was not authenticated with II when it registered its public key",
                ))?
            {
                return Err(String::from(
                    "caller is not the same as the one which registered the public key",
                ));
            }
            // call the on_message handler initialized in init()
            let handler_result = HANDLERS.with(|h| {
                // check if on_message method has been initialized during init()
                let on_message_handler = h.borrow().on_message.to_owned()?;
                // trigger the on_message handler initialized by canister
                on_message_handler(canister_message);
                Ok(())
            });
            return handler_result;
        },
        // WS Gateway relays a message from the client
        CanisterIncomingMessage::RelayedByGateway(client_msg) => {
            let WebsocketMessage {
                client_key,
                sequence_num,
                timestamp: _timestamp,
                message,
            } = from_slice(&client_msg.content).map_err(|e| e.to_string())?;

            // check if the signature is a Ed25519 signature
            let sig = Signature::from_slice(&client_msg.sig).map_err(|e| e.to_string())?;
            // check if client_key is a Ed25519 public key
            let public_key = PublicKey::from_slice(&client_key).map_err(|e| e.to_string())?;

            // check if client registered its public key by calling ws_register
            check_registered_client_key(&client_key)?;
            // check if the signature on the content of the client message verifies against the public key of the registered client
            // if so, the message came from the same client that registered its public key using ws_register
            public_key
                .verify(&client_msg.content, &sig)
                .map_err(|e| e.to_string())?;

            // check if the incoming message has the expected sequence number
            if sequence_num == get_expected_incoming_message_from_client_num(&client_key)? {
                // increse the expected sequence number by 1
                increment_expected_incoming_message_from_client_num(&client_key)?;
                // call the on_message handler initialized in init()
                let handler_result = HANDLERS.with(|h| {
                    // check if on_message method has been initialized during init()
                    let on_message_handler = h.borrow().on_message.to_owned()?;
                    // create messaeg to send to client
                    let canister_message = DirectClientMessage {
                        message,
                        client_key,
                    };
                    // trigger the on_message handler initialized by canister
                    on_message_handler(canister_message);
                    Ok(())
                });
                return handler_result;
            }
            Err(String::from(
                "incoming client's message relayed from WS Gateway does not have the expected sequence number",
            ))
        },
        // WS Gateway notifies the canister of the established IC WebSocket connection
        CanisterIncomingMessage::IcWebSocketEstablished(client_key) => {
            print(format!(
                "Can start notifying client with key: {:?}",
                client_key
            ));
            // call the on_open handler initialized in init()
            let handler_result = HANDLERS.with(|h| {
                // check if on_open method has been initialized during init()
                let on_open_handler = h.borrow().on_open.to_owned()?;
                // trigger the on_open handler initialized by canister
                on_open_handler(client_key);
                Ok(())
            });
            handler_result
        },
        // TODO: remove registered client when connection is closed
    }
}

/// Returns messages to the WS Gateway in response of a polling iteration.
pub fn ws_get_messages(args: CanisterWsGetMessagesArguments) -> CanisterWsGetMessagesResult {
    // check if the caller of this method is the WS Gateway that has been set during the initialization of the SDK
    check_caller_is_registered_gateway()?;

    get_cert_messages(args.nonce)
}

/// Sends a message to the client.
///
/// Under the hood, the message is serialized and certified, and then it is added to the queue of messages
/// that the WS Gateway will poll in the next iteration.
pub fn ws_send<'a, T: Deserialize<'a> + Serialize>(
    client_key: ClientPublicKey,
    msg: T,
) -> CanisterWsSendResult {
    // serialize the message for the client into msg_cbor
    let mut msg_cbor = vec![];
    let mut serializer = Serializer::new(&mut msg_cbor);
    serializer.self_describe().map_err(|e| e.to_string())?;
    msg.serialize(&mut serializer).map_err(|e| e.to_string())?;

    // get the principal of the gateway that is polling the canister
    let gateway = get_gateway_principal();

    let time = time();

    // the nonce in key is used by the WS Gateway to determine the message to start from in the next polling iteration
    // the key is also passed to the client in order to validate the body of the certified message
    let key = gateway.clone().to_string()
        + "_"
        + &format!("{:0>20}", next_outgoing_message_nonce().to_string());

    let input = WebsocketMessage {
        client_key: client_key.clone(),
        sequence_num: next_outgoing_message_to_client_num(&client_key)?,
        timestamp: time,
        message: msg_cbor,
    };

    // serialize the message of type WebsocketMessage into data
    let mut data = vec![];
    let mut serializer = Serializer::new(&mut data);
    serializer.self_describe().map_err(|e| e.to_string())?;
    input
        .serialize(&mut serializer)
        .map_err(|e| e.to_string())?;

    // certify data
    put_cert_for_message(key.clone(), &data);

    MESSAGES_FOR_GATEWAY.with(|m| {
        // messages in the queue are inserted with contiguous and increasing nonces
        // (from beginning to end of the queue) as ws_send is called sequentially, the nonce
        // is incremented by one in each call, and the message is pushed at the end of the queue
        m.borrow_mut().push_back(CanisterOutputMessage {
            client_key,
            key,
            val: data,
        });
    });
    Ok(())
}

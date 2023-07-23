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
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 50;

pub type ClientPublicKey = Vec<u8>;

/// The result of `ws_register`.
pub type CanisterWsRegisterResult = Result<(), String>;
/// The result of `ws_open`.
pub type CanisterWsOpenResult = Result<(Vec<u8>, Principal), String>;
/// The result of `ws_message`.
pub type CanisterWsMessageResult = Result<(), String>;
/// The result of `ws_get_messages`.
pub type CanisterWsGetMessagesResult = Result<CanisterOutputCertifiedMessages, String>;
/// The result of `ws_send`.
pub type CanisterWsSendResult = Result<(), String>;
/// The result of `ws_close`.
pub type CanisterWsCloseResult = Result<(), String>;

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
    /// Maps the client's public key to the WS Gateway's identity.
    // TODO: fix the WS Gateway identity during init() (as we are assuming there is only one gateway polling the canister)
    static CLIENT_GATEWAY_MAP: RefCell<HashMap<ClientPublicKey, Principal>> = RefCell::new(HashMap::new());
    /// Maps the client's public key to the sequence number to use for the next outgoing message (to that client).
    static OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP: RefCell<HashMap<ClientPublicKey, u64>> = RefCell::new(HashMap::new());
    /// Maps the client's public key to the expected sequence number of the next incoming message (from that client).
    static INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP: RefCell<HashMap<ClientPublicKey, u64>> = RefCell::new(HashMap::new());
    /// Maps the principal of the WS Gateway to the messages that have to be sent to it.
    /// The WS Gateway specifies the index of the first message to get in the next polling iteration.
    // TODO: keep messages only for gateway specified in init()
    static GATEWAY_MESSAGES_MAP: RefCell<HashMap<Principal, VecDeque<CanisterOutputMessage>>> = RefCell::new(HashMap::new());
    /// Keeps track of the Merkle tree used for certified queries
    static CERT_TREE: RefCell<RbTree<String, ICHash>> = RefCell::new(RbTree::new());
    /// Keeps track of the nonce which:
    /// - the WS Gateway uses to specify the first index of the certified messages to be returned when polling
    /// - the client uses as part of the path in the Merkle tree in order to verify the certificate of the messages relayed by the WS Gateway
    static NEXT_MESSAGE_NONCE: RefCell<u64> = RefCell::new(0u64);
}

pub fn wipe() {
    CLIENT_CALLER_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    CLIENT_GATEWAY_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    GATEWAY_MESSAGES_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    CERT_TREE.with(|t| {
        t.replace(RbTree::new());
    });
    NEXT_MESSAGE_NONCE.with(|next_id| next_id.replace(0u64));
}

fn next_message_nonce() -> u64 {
    NEXT_MESSAGE_NONCE.with(|n| n.replace_with(|&mut old| old + 1))
}

fn put_client_caller(client_key: ClientPublicKey) {
    CLIENT_CALLER_MAP.with(|map| {
        map.borrow_mut().insert(client_key, caller());
    })
}

fn get_client_caller(client_key: &ClientPublicKey) -> Option<Principal> {
    CLIENT_CALLER_MAP.with(|map| Some(map.borrow().get(client_key)?.to_owned()))
}

fn put_client_gateway(client_key: ClientPublicKey) {
    CLIENT_GATEWAY_MAP.with(|map| {
        map.borrow_mut().insert(client_key, caller());
    })
}

pub fn get_client_gateway(client_key: &ClientPublicKey) -> Option<Principal> {
    CLIENT_GATEWAY_MAP.with(|map| map.borrow().get(client_key).cloned())
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
    // associate the identity of the WS Gateway to the public key of the client
    put_client_gateway(client_key.clone());
    // initialize incoming client's message sequence number to 0
    init_expected_incoming_message_from_client_num(client_key.clone());
    // initialize outgoing message sequence number to 0
    init_outgoing_message_to_client_num(client_key);
}

fn remove_client(client_key: ClientPublicKey) {
    CLIENT_CALLER_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
    CLIENT_GATEWAY_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
    OUTGOING_MESSAGE_TO_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
    INCOMING_MESSAGE_FROM_CLIENT_NUM_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
}

fn get_cert_messages(nonce: u64) -> CanisterOutputCertifiedMessages {
    GATEWAY_MESSAGES_MAP.with(|s| {
        let gateway = caller();

        let mut s = s.borrow_mut();
        let gateway_messages_vec = match s.get_mut(&gateway) {
            None => {
                s.insert(gateway.clone(), VecDeque::new());
                s.get_mut(&gateway).unwrap()
            },
            Some(map) => map,
        };

        let smallest_key =
            gateway.clone().to_string() + "_" + &format!("{:0>20}", nonce.to_string());
        let start_index = gateway_messages_vec.partition_point(|x| x.key < smallest_key);
        let mut end_index = start_index;
        while (end_index < gateway_messages_vec.len())
            && (end_index < start_index + MAX_NUMBER_OF_RETURNED_MESSAGES)
        {
            end_index += 1;
        }
        let mut messages: Vec<CanisterOutputMessage> = Vec::with_capacity(end_index - start_index);
        for index in 0..(end_index - start_index) {
            messages.push(
                gateway_messages_vec
                    .get(start_index + index)
                    .unwrap()
                    .clone(),
            );
        }
        if end_index > start_index {
            let first_key = messages.first().unwrap().key.clone();
            let last_key = messages.last().unwrap().key.clone();
            let (cert, tree) = get_cert_for_range(&first_key, &last_key);
            CanisterOutputCertifiedMessages {
                messages,
                cert,
                tree,
            }
        } else {
            CanisterOutputCertifiedMessages {
                messages,
                cert: Vec::new(),
                tree: Vec::new(),
            }
        }
    })
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

// canister specifies the handlers that the CDK uses to manage the IC WebSocket connection
pub fn init(on_open: OnOpenCallback, on_message: OnMessageCallback, on_close: OnCloseCallback) {
    HANDLERS.with(|h| {
        let mut h = h.borrow_mut();
        h.on_open = Ok(on_open);
        h.on_message = Ok(on_message);
        h.on_close = Ok(on_close);
    });
}

// register the public key that the client SDK has newly generated to initialize an IcWebSocket connection
pub fn ws_register(args: CanisterWsRegisterArguments) -> CanisterWsRegisterResult {
    // associate the identity of the client to its public key received as input
    put_client_caller(args.client_key);
    Ok(())
}

// WS Gateway relays the first message sent by the client together with its signature
// to prove that the first message is actually coming from the same client that registered its public key
// beforehand by calling ws_register()
pub fn ws_open(args: CanisterWsOpenArguments) -> CanisterWsOpenResult {
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

    Ok((client_key, canister_id))
}

// closes the IC WebSocket connection with a client
pub fn ws_close(args: CanisterWsCloseArguments) -> CanisterWsCloseResult {
    remove_client(args.client_key.clone());

    HANDLERS.with(|h| {
        let on_close_handler = h.borrow().on_close.to_owned()?;
        on_close_handler(args.client_key);
        Ok(())
    })
}

// handles the messages received by the canister
pub fn ws_message(args: CanisterWsMessageArguments) -> CanisterWsMessageResult {
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

// gets messages that need to be sent to the WS Gateway in response of a polling iteration
pub fn ws_get_messages(args: CanisterWsGetMessagesArguments) -> CanisterWsGetMessagesResult {
    Ok(get_cert_messages(args.nonce))
}

// messages that the canister wants to send to some client are stored in GATEWAY_MESSAGES_MAP
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
    let gateway = get_client_gateway(&client_key)
        .ok_or(String::from("client has no corresponding gateway in map"))?;

    let time = time();

    // the nonce in key is used by the WS Gateway to determine the message to start from in the next polling iteration
    // the key is also passed to the client in order to validate the body of the certified message
    let key =
        gateway.clone().to_string() + "_" + &format!("{:0>20}", next_message_nonce().to_string());

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

    // TODO: init GATEWAY_MESSAGES_MAP with one gateway principal during init()
    GATEWAY_MESSAGES_MAP.with(|s| {
        let mut s = s.borrow_mut();
        let gw_map = match s.get_mut(&gateway) {
            None => {
                s.insert(gateway.clone(), VecDeque::new());
                s.get_mut(&gateway).unwrap()
            },
            Some(map) => map,
        };
        // messages in the queue are inserted with contiguous and increasing nonces
        // (from beginning to end of the queue) as ws_send is called sequentially, the nonce
        // is incremented by one in each call, and the message is pushed at the end of the queue
        gw_map.push_back(CanisterOutputMessage {
            client_key,
            key,
            val: data,
        });
    });
    Ok(())
}

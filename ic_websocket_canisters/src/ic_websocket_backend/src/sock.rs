use candid::{CandidType, Principal};
use ed25519_compact::{PublicKey, Signature};
use ic_cdk::api::{caller, data_certificate, set_certified_data, time};
use ic_cdk::print;
use ic_certified_map::{labeled, labeled_hash, AsHashTree, Hash as ICHash, RbTree};
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, Serializer};
use sha2::{Digest, Sha256};
use std::ops::Add;
use std::{
    cell::RefCell, collections::HashMap, collections::VecDeque, convert::AsRef, time::Duration,
};

const LABEL_WEBSOCKET: &[u8] = b"websocket";
const MSG_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 50;

pub type PublicKeySlice = Vec<u8>;
pub type WsOpenResult = Result<(Vec<u8>, Principal), String>;
pub type WsMessageResult = Result<(), String>;
pub type WsCloseResult = Result<(), String>;

pub struct KeyGatewayTime {
    key: String,
    gateway: Principal,
    time: u64,
}

// The first message used in ws_open().
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
struct FirstMessage {
    #[serde(with = "serde_bytes")]
    client_key: PublicKeySlice,
    canister_id: Principal,
}

// Encoded message + signature from client.
#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct ClientMessage {
    #[serde(with = "serde_bytes")]
    content: Vec<u8>,
    #[serde(with = "serde_bytes")]
    sig: Vec<u8>,
}

#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub enum GatewayMessage {
    RelayedFromClient(ClientMessage),
    IcWebSocketEstablished(PublicKeySlice),
}

pub struct CanisterMessage {
    pub message: Vec<u8>,
    pub client_key: PublicKeySlice,
}

// Messages have the following required fields (both ways).
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct WebsocketMessage {
    #[serde(with = "serde_bytes")]
    pub client_key: PublicKeySlice, // To or from client key.
    pub sequence_num: u64, // Both ways, messages should arrive with sequence numbers 0, 1, 2...
    pub timestamp: u64,    // Timestamp of when the message was made for the recipient to inspect.
    #[serde(with = "serde_bytes")]
    pub message: Vec<u8>, // Application message encoded in binary.
}

// One message in the list returned to the gateway polling for messages.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct EncodedMessage {
    #[serde(with = "serde_bytes")]
    client_key: PublicKeySlice, // The client that the gateway will forward the message to.
    key: String, // Key for certificate verification.
    #[serde(with = "serde_bytes")]
    val: Vec<u8>, // Encoded WebsocketMessage.
}

// List of messages returned to the polling gateway.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct CertMessages {
    messages: Vec<EncodedMessage>, // List of messages.
    #[serde(with = "serde_bytes")]
    cert: Vec<u8>, // cert+tree constitute the certificate for all returned messages.
    #[serde(with = "serde_bytes")]
    tree: Vec<u8>, // cert+tree constitute the certificate for all returned messages.
}

thread_local! {
    static CLIENT_CALLER_MAP: RefCell<HashMap<PublicKeySlice, Principal>> = RefCell::new(HashMap::new());
    static CLIENT_GATEWAY_MAP: RefCell<HashMap<PublicKeySlice, Principal>> = RefCell::new(HashMap::new());
    static CLIENT_MESSAGE_NUM_MAP: RefCell<HashMap<PublicKeySlice, u64>> = RefCell::new(HashMap::new());
    static CLIENT_INCOMING_NUM_MAP: RefCell<HashMap<PublicKeySlice, u64>> = RefCell::new(HashMap::new());
    static GATEWAY_MESSAGES_MAP: RefCell<HashMap<Principal, VecDeque<EncodedMessage>>> = RefCell::new(HashMap::new());
    static MESSAGE_DELETE_QUEUE: RefCell<VecDeque<KeyGatewayTime>> = RefCell::new(VecDeque::new());
    static CERT_TREE: RefCell<RbTree<String, ICHash>> = RefCell::new(RbTree::new());
    static NEXT_MESSAGE_NONCE: RefCell<u64> = RefCell::new(0u64);
}

pub fn wipe() {
    CLIENT_CALLER_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    CLIENT_GATEWAY_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    CLIENT_MESSAGE_NUM_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    CLIENT_INCOMING_NUM_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    GATEWAY_MESSAGES_MAP.with(|map| {
        map.borrow_mut().clear();
    });
    MESSAGE_DELETE_QUEUE.with(|vd| {
        vd.borrow_mut().clear();
    });
    CERT_TREE.with(|t| {
        t.replace(RbTree::new());
    });
    NEXT_MESSAGE_NONCE.with(|next_id| next_id.replace(0u64));
}

fn next_message_nonce() -> u64 {
    NEXT_MESSAGE_NONCE.with(|n| n.replace_with(|&mut old| old + 1))
}

fn put_client_caller(client_key: PublicKeySlice) {
    CLIENT_CALLER_MAP.with(|map| {
        map.borrow_mut().insert(client_key, caller());
    })
}

fn put_client_gateway(client_key: PublicKeySlice) {
    CLIENT_GATEWAY_MAP.with(|map| {
        map.borrow_mut().insert(client_key, caller());
    })
}

pub fn get_client_gateway(client_key: &PublicKeySlice) -> Option<Principal> {
    CLIENT_GATEWAY_MAP.with(|map| map.borrow().get(client_key).cloned())
}

fn check_registered_client_key(client_key: &PublicKeySlice) -> Result<(), String> {
    match CLIENT_CALLER_MAP.with(|map| map.borrow().contains_key(client_key)) {
        true => Ok(()),
        false => Err(String::from("client's public key has not been previously registered by client"))
    }
}

pub fn next_client_message_num(client_key: &PublicKeySlice) -> u64 {
    CLIENT_MESSAGE_NUM_MAP.with(|map| {
        map.borrow_mut().get_mut(client_key).expect("message number for client key should be initialized").add(1)
    })
}

fn get_client_incoming_num(client_key: &PublicKeySlice) -> u64 {
    CLIENT_INCOMING_NUM_MAP.with(|map| *map.borrow().get(client_key).unwrap_or(&0))
}

fn increase_expected_client_incoming_num(client_key: &PublicKeySlice) -> Result<u64, String> {
    CLIENT_INCOMING_NUM_MAP.with(|map| {
        match map.borrow_mut().get_mut(client_key) {
            Some(num) => Ok(num.add(1)),
            None => Err(String::from("next client sequence number not correctly initialized")),
        }
    })
}

fn add_client(client_key: PublicKeySlice) {
    // associate the identity of the WS Gateway to the public key of the client
    put_client_gateway(client_key.clone());
    // initialize incoming client's message sequence number to 0
    get_client_incoming_num(&client_key);
    // initialize outgoing message sequence number to 0
    CLIENT_MESSAGE_NUM_MAP.with(|map| {
        map.borrow_mut().insert(client_key, 0);
    });
}

fn remove_client(client_key: PublicKeySlice) {
    CLIENT_CALLER_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
    CLIENT_GATEWAY_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
    CLIENT_MESSAGE_NUM_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
    CLIENT_INCOMING_NUM_MAP.with(|map| {
        map.borrow_mut().remove(&client_key);
    });
}

fn get_cert_messages(nonce: u64) -> CertMessages {
    GATEWAY_MESSAGES_MAP.with(|s| {
        let gateway = caller();

        let mut s = s.borrow_mut();
        let gateway_messages_vec = match s.get_mut(&gateway) {
            None => {
                s.insert(gateway.clone(), VecDeque::new());
                s.get_mut(&gateway).unwrap()
            }
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
        let mut messages: Vec<EncodedMessage> = Vec::with_capacity(end_index - start_index);
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
            CertMessages {
                messages,
                cert,
                tree,
            }
        } else {
            CertMessages {
                messages,
                cert: Vec::new(),
                tree: Vec::new(),
            }
        }
    })
}

fn delete_message(message_info: &KeyGatewayTime) {
    GATEWAY_MESSAGES_MAP.with(|s| {
        let mut s = s.borrow_mut();
        let gateway_messages = s.get_mut(&message_info.gateway).unwrap();
        gateway_messages.pop_front();
    });
    CERT_TREE.with(|t| {
        t.borrow_mut().delete(message_info.key.as_ref());
    });
}

fn put_cert_for_message(key: String, value: &Vec<u8>) {
    let root_hash = CERT_TREE.with(|tree| {
        let mut tree = tree.borrow_mut();
        tree.insert(key.clone(), Sha256::digest(value).into());
        labeled_hash(LABEL_WEBSOCKET, &tree.root_hash())
    });

    set_certified_data(&root_hash);
}

// fn get_cert_for_message(key: &String) -> (Vec<u8>, Vec<u8>) {
//     CERT_TREE.with(|tree| {
//         let tree = tree.borrow();
//         let witness = tree.witness(key.as_ref());
//         let tree = labeled(LABEL_WEBSOCKET, witness);

//         let mut data = vec![];
//         let mut serializer = Serializer::new(&mut data);
//         serializer.self_describe().unwrap();
//         tree.serialize(&mut serializer).unwrap();
//         (data_certificate().unwrap(), data)
//     })
// }

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

type OnOpenCallback = fn(PublicKeySlice);
type OnMessageCallback = fn(CanisterMessage);
type OnCloseCallback = fn(PublicKeySlice);

struct Handlers {
    on_open: Result<OnOpenCallback, String>,
    on_message: Result<OnMessageCallback, String>,
    on_close: Result<OnCloseCallback, String>,
}

thread_local! {
    /* flexible */ static HANDLERS: RefCell<Handlers> = RefCell::new(Handlers {
        on_open: Err(String::from("on_open handler not initialized")),
        on_message: Err(String::from("on_message handler not initialized")),
        on_close: Err(String::from("on_close handler not initialized")),
    });
}

pub fn init(on_open: OnOpenCallback, on_message: OnMessageCallback, on_close: OnCloseCallback) {
    HANDLERS.with(|h| {
        let mut h = h.borrow_mut();
        h.on_open = Ok(on_open);
        h.on_message = Ok(on_message);
        h.on_close = Ok(on_close);
    });
}

pub fn ws_register(client_key: PublicKeySlice) {
    // associate the identity of the client to its public key received as input
    put_client_caller(client_key)
}

pub fn ws_open(msg: Vec<u8>, sig: Vec<u8>) -> WsOpenResult {
    // check if the message relayed by the WS Gateway is of type FirstMessage
    let FirstMessage { client_key, canister_id } = from_slice(&msg).map_err(|e| e.to_string())?;
    // check if client_key is a Ed25519 public key
    let public_key = PublicKey::from_slice(&client_key).map_err(|e| e.to_string())?;
    // check if the signature realyed by the WS Gateway is a Ed25519 signature
    let sig = Signature::from_slice(&sig).map_err(|e| e.to_string())?;

    // check if client registered its public key by calling ws_register
    check_registered_client_key(&client_key)?;
    // check if the signature on FirstMessage verifies against the public key of the registered client
    // if so, the first message came from the same client that registered its public key using ws_register
    public_key.verify(&msg, &sig).map_err(|e| e.to_string())?;

    // initialize client maps
    add_client(client_key.clone());

    Ok((client_key, canister_id))
}

pub fn ws_close(client_key: PublicKeySlice) -> WsCloseResult {
    remove_client(client_key.clone());

    HANDLERS.with(|h| {
        let on_close_handler = h.borrow().on_close.to_owned()?;
        on_close_handler(client_key);
        Ok(())
    })
}

pub fn ws_message(msg: GatewayMessage) -> WsMessageResult {
    match msg {
        // WS Gateway relays a message from the client
        GatewayMessage::RelayedFromClient(client_msg) => {
            let WebsocketMessage {
                client_key,
                sequence_num,
                timestamp: _timestamp,
                message
            } = from_slice(&client_msg.content).map_err(|e| e.to_string())?;

            // check if the signature is a Ed25519 signature
            let sig = Signature::from_slice(&client_msg.sig).map_err(|e| e.to_string())?;
            // check if client_key is a Ed25519 public key
            let public_key = PublicKey::from_slice(&client_key).map_err(|e| e.to_string())?;

            // check if client registered its public key by calling ws_register
            check_registered_client_key(&client_key)?;
            // check if the signature on the content of ClientMessage verifies against the public key of the registered client
            // if so, the message came from the same client that registered its public key using ws_register
            public_key.verify(&client_msg.content, &sig).map_err(|e| e.to_string())?;
            
            // check if the incoming message has the expected sequence number
            if sequence_num == get_client_incoming_num(&client_key) {
                // increse the expected sequence number by 1
                let next_seq_num = increase_expected_client_incoming_num(&client_key)?;
                println!("Next sequence number: {}", next_seq_num);
                // call the on_message handler initialized in init()
                let handler_result = HANDLERS.with(|h| {
                    // check if on_message method has been initialized during init()
                    let on_message_handler = h.borrow().on_message.to_owned()?;
                    // create messaeg to send to client
                    let canister_message = CanisterMessage {
                        message,
                        client_key,
                    };
                    // trigger the on_message handler initialized by canister
                    on_message_handler(canister_message);
                    Ok(())
                });
                return handler_result;
            }
            Err(String::from("incoming client's message relayed from WS Gateway does not have the expected sequence number"))
        },
        // WS Gateway notifies the canister of the established IC WebSocket connection
        GatewayMessage::IcWebSocketEstablished(client_key) => {
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
        }
        // TODO: remove registered client when connection is closed
    }
}

pub fn ws_get_messages(nonce: u64) -> CertMessages {
    get_cert_messages(nonce)
}

pub fn ws_send<'a, T: Deserialize<'a> + Serialize>(client_key: PublicKeySlice, msg: T) {
    // serialize the message into msg_cbor
    let mut msg_cbor = vec![];
    let mut serializer = Serializer::new(&mut msg_cbor);
    serializer.self_describe().unwrap();
    msg.serialize(&mut serializer).unwrap();

    let gateway = match get_client_gateway(&client_key) {
        None => {
            return;
        }
        Some(gateway) => gateway,
    };

    let time = time();
    let key =
        gateway.clone().to_string() + "_" + &format!("{:0>20}", next_message_nonce().to_string());

    MESSAGE_DELETE_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        q.push_back(KeyGatewayTime {
            key: key.clone(),
            gateway: gateway.clone(),
            time,
        });

        let front = q.front().unwrap();
        if Duration::from_nanos(time - front.time) > MSG_TIMEOUT {
            delete_message(front);
            q.pop_front();

            let front = q.front().unwrap();
            if Duration::from_nanos(time - front.time) > MSG_TIMEOUT {
                delete_message(front);
                q.pop_front();
            }
        }
    });

    let input = WebsocketMessage {
        client_key: client_key.clone(),
        sequence_num: next_client_message_num(&client_key),
        timestamp: time,
        message: msg_cbor,
    };

    let mut data = vec![];
    let mut serializer = Serializer::new(&mut data);
    serializer.self_describe().unwrap();
    input.serialize(&mut serializer).unwrap();

    put_cert_for_message(key.clone(), &data);
    GATEWAY_MESSAGES_MAP.with(|s| {
        let mut s = s.borrow_mut();
        let gw_map = match s.get_mut(&gateway) {
            None => {
                s.insert(gateway.clone(), VecDeque::new());
                s.get_mut(&gateway).unwrap()
            }
            Some(map) => map,
        };
        gw_map.push_back(EncodedMessage {
            client_key,
            key,
            val: data,
        });
    });
}

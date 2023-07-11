use candid::{CandidType, Principal};
use ed25519_compact::{PublicKey, Signature};
use ic_cdk::api::{caller, data_certificate, set_certified_data, time};
use ic_cdk::print;
use ic_certified_map::{labeled, labeled_hash, AsHashTree, Hash as ICHash, RbTree};
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, Serializer};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell, collections::HashMap, collections::VecDeque, convert::AsRef, time::Duration,
};

const LABEL_WEBSOCKET: &[u8] = b"websocket";
const MSG_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 50;

pub type PublicKeySlice = Vec<u8>;

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
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
struct ClientMessage {
    #[serde(with = "serde_bytes")]
    val: Vec<u8>,
    #[serde(with = "serde_bytes")]
    sig: Vec<u8>,
}

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
enum GatewayMessage {
    RelayedFromClient(ClientMessage),
    FromGateway(PublicKeySlice, bool),
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

fn check_registered_client_key(client_key: &PublicKeySlice) -> bool {
    CLIENT_CALLER_MAP.with(|map| map.borrow().contains_key(client_key))
}

pub fn next_client_message_num(client_key: PublicKeySlice) -> u64 {
    CLIENT_MESSAGE_NUM_MAP.with(|map| {
        let mut map = map.borrow_mut();
        match map.get(&client_key).cloned() {
            None => {
                map.insert(client_key, 0);
                0
            }
            Some(num) => {
                map.insert(client_key, num + 1);
                num + 1
            }
        }
    })
}

fn get_client_incoming_num(client_key: PublicKeySlice) -> u64 {
    CLIENT_INCOMING_NUM_MAP.with(|map| *map.borrow().get(&client_key).unwrap_or(&0))
}

fn put_client_incoming_num(client_key: PublicKeySlice, num: u64) {
    CLIENT_INCOMING_NUM_MAP.with(|map| {
        map.borrow_mut().insert(client_key, num);
    })
}

fn delete_client(client_key: PublicKeySlice) {
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
type OnMessageCallback = fn(WebsocketMessage);
type OnCloseCallback = fn(PublicKeySlice);

struct Handlers {
    on_open: Option<OnOpenCallback>,
    on_message: Option<OnMessageCallback>,
    on_close: Option<OnCloseCallback>,
}

thread_local! {
    /* flexible */ static HANDLERS: RefCell<Handlers> = RefCell::new(Handlers {
        on_open: None,
        on_message: None,
        on_close: None,
    });
}

pub fn init(on_open: OnOpenCallback, on_message: OnMessageCallback, on_close: OnCloseCallback) {
    HANDLERS.with(|h| {
        let mut h = h.borrow_mut();
        h.on_open = Some(on_open);
        h.on_message = Some(on_message);
        h.on_close = Some(on_close);
    });
}

// Client submits its public key and gets a new client_key back.
pub fn ws_register(client_key: PublicKeySlice) {
    // The identity (caller) used in this update call will be associated with this client_key. Remember this identity.
    put_client_caller(client_key)
}

pub fn ws_open(msg: Vec<u8>, sig: Vec<u8>) -> bool {
    let decoded: FirstMessage = from_slice(&msg).unwrap();

    let client_key = decoded.client_key;

    if check_registered_client_key(&client_key) {
        let sig = Signature::from_slice(&sig).unwrap();

        return {
            match PublicKey::from_slice(&client_key)
                .unwrap()
                .verify(&msg, &sig)
            {
                Ok(_) => {
                    // Remember this gateway will get the messages for this client_key.
                    put_client_gateway(client_key.clone());
                    true
                }
                Err(_) => false,
            }
        };
    }
    false
}

pub fn ws_close(client_key: PublicKeySlice) {
    delete_client(client_key.clone());

    HANDLERS.with(|h| {
        if let Some(on_close) = h.borrow().on_close {
            on_close(client_key);
        }
    })
}

pub fn ws_message(msg: Vec<u8>) -> bool {
    let decoded: GatewayMessage = from_slice(&msg).unwrap();
    match decoded {
        GatewayMessage::RelayedFromClient(decoded_msg) => {
            let content: WebsocketMessage = from_slice(&decoded_msg.val).unwrap();

            // Verify the signature.
            let sig = Signature::from_slice(&decoded_msg.sig).unwrap();
            let valid = PublicKey::from_slice(&content.client_key)
                .unwrap()
                .verify(&decoded_msg.val, &sig);

            match valid {
                Ok(_) => {
                    // Verify the message sequence number.
                    if content.sequence_num == get_client_incoming_num(content.client_key.clone()) {
                        put_client_incoming_num(
                            content.client_key.clone(),
                            content.sequence_num + 1,
                        );
                        HANDLERS.with(|h| {
                            if let Some(on_message) = h.borrow().on_message {
                                on_message(content);
                            }
                        });
                        true
                    } else {
                        false
                    }
                }
                Err(_) => false,
            }
        }
        GatewayMessage::FromGateway(client_key, can_send) => {
            if can_send {
                print(format!(
                    "Can start notifying client with key: {:?}",
                    client_key
                ));
                HANDLERS.with(|h| {
                    if let Some(on_open) = h.borrow().on_open {
                        on_open(client_key);
                    }
                });
            } else {
                // TODO: remove registered client
            }
            can_send
        }
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
        sequence_num: next_client_message_num(client_key.clone()),
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

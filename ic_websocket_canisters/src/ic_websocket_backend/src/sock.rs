use ic_cdk::api::{caller, data_certificate, set_certified_data, time};
use ic_certified_map::{labeled, labeled_hash, AsHashTree, Hash as ICHash, RbTree};
use serde::Serialize;
use serde_cbor::Serializer;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell, collections::HashMap, collections::VecDeque, convert::AsRef, time::Duration,
};

use crate::{CertMessages, EncodedMessage, WebsocketMessage, PublicKeySlice};

const LABEL_WEBSOCKET: &[u8] = b"websocket";
const MSG_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 50;

pub struct KeyGatewayTime {
    key: String,
    gateway: String,
    time: u64,
}

thread_local! {
    static CLIENT_CALLER_MAP: RefCell<HashMap<PublicKeySlice, String>> = RefCell::new(HashMap::new());
    static CLIENT_GATEWAY_MAP: RefCell<HashMap<PublicKeySlice, String>> = RefCell::new(HashMap::new());
    static CLIENT_MESSAGE_NUM_MAP: RefCell<HashMap<PublicKeySlice, u64>> = RefCell::new(HashMap::new());
    static CLIENT_INCOMING_NUM_MAP: RefCell<HashMap<PublicKeySlice, u64>> = RefCell::new(HashMap::new());
    static GATEWAY_MESSAGES_MAP: RefCell<HashMap<String, VecDeque<EncodedMessage>>> = RefCell::new(HashMap::new());
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

pub fn put_client_caller(client_key: PublicKeySlice) {
    CLIENT_CALLER_MAP.with(|map| {
        map.borrow_mut().insert(client_key, caller().to_string());
    })
}

pub fn put_client_gateway(client_key: PublicKeySlice) {
    CLIENT_GATEWAY_MAP.with(|map| {
        map.borrow_mut().insert(client_key, caller().to_string());
    })
}

pub fn get_client_gateway(client_key: &PublicKeySlice) -> Option<String> {
    CLIENT_GATEWAY_MAP.with(|map| map.borrow().get(client_key).cloned())
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

pub fn get_client_incoming_num(client_key: PublicKeySlice) -> u64 {
    CLIENT_INCOMING_NUM_MAP.with(|map| *map.borrow().get(&client_key).unwrap_or(&0))
}

pub fn put_client_incoming_num(client_key: PublicKeySlice, num: u64) {
    CLIENT_INCOMING_NUM_MAP.with(|map| {
        map.borrow_mut().insert(client_key, num);
    })
}

pub fn delete_client(client_key: PublicKeySlice) {
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

pub fn get_cert_messages(nonce: u64) -> CertMessages {
    GATEWAY_MESSAGES_MAP.with(|s| {
        let gateway = caller().to_string();

        let mut s = s.borrow_mut();
        let gateway_messages_vec = match s.get_mut(&gateway) {
            None => {
                s.insert(gateway.clone(), VecDeque::new());
                s.get_mut(&gateway).unwrap()
            }
            Some(map) => map,
        };

        let smallest_key = gateway.clone() + "_" + &format!("{:0>20}", nonce.to_string());
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

pub fn delete_message(message_info: &KeyGatewayTime) {
    GATEWAY_MESSAGES_MAP.with(|s| {
        let mut s = s.borrow_mut();
        let gateway_messages = s.get_mut(&message_info.gateway).unwrap();
        gateway_messages.pop_front();
    });
    CERT_TREE.with(|t| {
        t.borrow_mut().delete(message_info.key.as_ref());
    });
}

pub fn send_message_from_canister(client_key: PublicKeySlice, msg: Vec<u8>) {
    let gateway = match get_client_gateway(&client_key) {
        None => {
            return;
        }
        Some(gateway) => gateway,
    };

    let time = time();
    let key = gateway.clone() + "_" + &format!("{:0>20}", next_message_nonce().to_string());

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
        message: msg,
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

fn put_cert_for_message(key: String, value: &Vec<u8>) {
    let root_hash = CERT_TREE.with(|tree| {
        let mut tree = tree.borrow_mut();
        tree.insert(key.clone(), Sha256::digest(value).into());
        labeled_hash(LABEL_WEBSOCKET, &tree.root_hash())
    });

    set_certified_data(&root_hash);
}

pub fn get_cert_for_message(key: &String) -> (Vec<u8>, Vec<u8>) {
    CERT_TREE.with(|tree| {
        let tree = tree.borrow();
        let witness = tree.witness(key.as_ref());
        let tree = labeled(LABEL_WEBSOCKET, witness);

        let mut data = vec![];
        let mut serializer = Serializer::new(&mut data);
        serializer.self_describe().unwrap();
        tree.serialize(&mut serializer).unwrap();
        (data_certificate().unwrap(), data)
    })
}

pub fn get_cert_for_range(first: &String, last: &String) -> (Vec<u8>, Vec<u8>) {
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

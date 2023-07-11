use candid::Principal;
use ed25519_compact::{PublicKey, Signature};
use ic_cdk::export::candid::CandidType;
use ic_cdk::print;
use ic_cdk_macros::*;
use serde::{Deserialize, Serialize};
use serde_cbor::from_slice;

use canister::ws_on_message;
use canister::ws_on_open;
use sock::check_registered_client_key;
use sock::get_cert_messages;
use sock::get_client_incoming_num;
use sock::put_client_incoming_num;
use sock::{
    delete_client, put_client_caller, put_client_gateway,
    wipe,
};

pub mod canister;
pub mod sock;

pub type PublicKeySlice = Vec<u8>;

// Debug method. Wipes all data in the canister.
#[update]
fn ws_wipe() {
    wipe();
}

// Messages have the following required fields (both ways).
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct WebsocketMessage {
    #[serde(with = "serde_bytes")]
    pub client_key: PublicKeySlice,    // To or from client key.
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
    key: String,    // Key for certificate verification.
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

// Client submits its public key and gets a new client_key back.
#[update]
fn ws_register(client_key: PublicKeySlice) {
    // The identity (caller) used in this update call will be associated with this client_key. Remember this identity.
    put_client_caller(client_key);
}

// The first message used in ws_open().
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
struct FirstMessage {
    #[serde(with = "serde_bytes")]
    client_key: PublicKeySlice,
    canister_id: Principal,
}

// Open the websocket connection.
#[update]
fn ws_open(msg: Vec<u8>, sig: Vec<u8>) -> bool {
    let decoded: FirstMessage = from_slice(&msg).unwrap();

    let client_key = decoded.client_key;

    if check_registered_client_key(&client_key) {
        let sig = Signature::from_slice(&sig).unwrap();

        return {
            match PublicKey::from_slice(&client_key).unwrap().verify(&msg, &sig) {
                Ok(_) => {
                    // Remember this gateway will get the messages for this client_key.
                    put_client_gateway(client_key.clone());
                    true
                }
                Err(_) => false,
            }
        }
    }
    false
}

// Close the websocket connection.
#[update]
fn ws_close(client_key: Vec<u8>) {
    delete_client(client_key);
}

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
enum GatewayMessage {
    RelayedFromClient(ClientMessage),
    FromGateway(Vec<u8>, bool)
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

// Gateway calls this method to pass on the message from the client to the canister.
#[update]
fn ws_message(msg: Vec<u8>) -> bool {
    let decoded: GatewayMessage = from_slice(&msg).unwrap();
    match decoded {
        GatewayMessage::RelayedFromClient(decoded_msg) => {
            let content: WebsocketMessage = from_slice(&decoded_msg.val).unwrap();

            // Verify the signature.
            let sig = Signature::from_slice(&decoded_msg.sig).unwrap();
            let valid = PublicKey::from_slice(&content.client_key).unwrap().verify(&decoded_msg.val, &sig);
        
            match valid {
                Ok(_) => {
                    // Verify the message sequence number.
                    if content.sequence_num == get_client_incoming_num(content.client_key.clone()) {
                        put_client_incoming_num(content.client_key.clone(), content.sequence_num + 1);
                        ws_on_message(content);
                        true
                    } else {
                        false
                    }
                }
                Err(_) => false,
            }
        },
        GatewayMessage::FromGateway(client_key, can_send) => {
            if can_send {
                print(format!("Can start notifying client with key: {:?}", client_key));
                ws_on_open(client_key);
            }
            else {
                // TODO: remove registered client 
            }
            can_send
        }
    }
}

// Gateway polls this method to get messages for all the clients it serves.
#[query]
fn ws_get_messages(nonce: u64) -> CertMessages {
    get_cert_messages(nonce)
}

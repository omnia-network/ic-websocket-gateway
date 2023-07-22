use ic_cdk::{export::candid::CandidType, print};
use serde::{Deserialize, Serialize};
use serde_cbor::from_slice;

use crate::sock::{ws_send, ClientPublicKey, DirectClientMessage};

#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct AppMessage {
    pub text: String,
}

pub fn on_open(client_key: ClientPublicKey) {
    let msg = AppMessage {
        text: String::from("ping"),
    };
    send_app_message(client_key, msg);
}

pub fn on_message(msg: DirectClientMessage) {
    let app_msg: AppMessage = from_slice(&msg.message).unwrap();
    let new_msg = AppMessage {
        text: app_msg.clone().text + " ping",
    };
    print(format!("Received message: {:?}", app_msg));
    send_app_message(msg.client_key, new_msg)
}

fn send_app_message(client_key: ClientPublicKey, msg: AppMessage) {
    print(format!("Sending message: {:?}", msg));
    if let Err(e) = ws_send(client_key, msg) {
        println!("Could not send message: {}", e);
    }
}

pub fn on_close(client_key: ClientPublicKey) {
    print(format!("Client {:?} disconnected", client_key));
}

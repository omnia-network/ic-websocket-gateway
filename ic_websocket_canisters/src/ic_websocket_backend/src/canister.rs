use ic_cdk::{export::candid::CandidType, print};
use serde::{Deserialize, Serialize};
use serde_cbor::from_slice;

use crate::sock::{ws_send, PublicKeySlice, WebsocketMessage};

#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct AppMessage {
    pub text: String,
}

pub fn on_open(client_key: PublicKeySlice) {
    let msg = AppMessage {
        text: String::from("ping"),
    };
    send_app_message(client_key, msg);
}

pub fn on_message(content: WebsocketMessage) {
    let app_msg: AppMessage = from_slice(&content.message).unwrap();
    let new_msg = AppMessage {
        text: app_msg.clone().text + " ping",
    };
    print(format!("Received message: {:?}", app_msg));
    send_app_message(content.client_key, new_msg)
}

fn send_app_message(client_key: PublicKeySlice, msg: AppMessage) {
    print(format!("Sending message: {:?}", msg));
    ws_send(client_key, msg);
}

pub fn on_close(client_key: PublicKeySlice) {
    print(format!("Client {:?} disconnected", client_key));
}

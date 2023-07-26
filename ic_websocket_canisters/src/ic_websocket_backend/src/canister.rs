use ic_cdk::{export::candid::CandidType, print};
use serde::{Deserialize, Serialize};
use serde_cbor::from_slice;

use crate::sock::{
    ws_send, ClientPublicKey, OnCloseCallbackArgs, OnMessageCallbackArgs, OnOpenCallbackArgs,
};

pub const GATEWAY_PRINCIPAL: &str =
    "sqdfl-mr4km-2hfjy-gajqo-xqvh7-hf4mf-nra4i-3it6l-neaw4-soolw-tae";

#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct AppMessage {
    pub text: String,
}

pub fn on_open(args: OnOpenCallbackArgs) {
    let msg = AppMessage {
        text: String::from("ping"),
    };
    send_app_message(args.client_key, msg);
}

pub fn on_message(args: OnMessageCallbackArgs) {
    let app_msg: AppMessage = from_slice(&args.message).unwrap();
    let new_msg = AppMessage {
        text: app_msg.clone().text + " ping",
    };
    print(format!("Received message: {:?}", app_msg));
    send_app_message(args.client_key, new_msg)
}

fn send_app_message(client_key: ClientPublicKey, msg: AppMessage) {
    print(format!("Sending message: {:?}", msg));
    if let Err(e) = ws_send(client_key, msg) {
        println!("Could not send message: {}", e);
    }
}

pub fn on_close(args: OnCloseCallbackArgs) {
    print(format!("Client {:?} disconnected", args.client_key));
}

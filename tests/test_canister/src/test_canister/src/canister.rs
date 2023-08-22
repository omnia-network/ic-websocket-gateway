use candid::{decode_one, encode_one, CandidType};
use ic_cdk::{api::time, print};
use serde::{Deserialize, Serialize};

use ic_websocket_cdk::{
    ws_send, ClientPublicKey, OnCloseCallbackArgs, OnMessageCallbackArgs, OnOpenCallbackArgs,
};

use crate::CLIENTS_CONNECTED;

pub const GATEWAY_PRINCIPAL: &str =
    "sqdfl-mr4km-2hfjy-gajqo-xqvh7-hf4mf-nra4i-3it6l-neaw4-soolw-tae";

#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct AppMessage {
    pub text: String,
    /// Used in load tests to measure latency from canister to client.
    pub timestamp: u64,
}

impl AppMessage {
    fn candid_serialize(&self) -> Vec<u8> {
        encode_one(&self).unwrap()
    }
}

pub fn on_open(args: OnOpenCallbackArgs) {
    // add client to the list of connected clients
    CLIENTS_CONNECTED.with(|clients_connected| {
        clients_connected
            .borrow_mut()
            .insert(args.client_key.clone());

        print(format!(
            "[on_open] # clients connected: {}",
            clients_connected.borrow().len()
        ));
    });

    let msg = AppMessage {
        text: String::from("ping"),
        timestamp: time(),
    };
    send_app_message(args.client_key, msg);
}

pub fn on_message(args: OnMessageCallbackArgs) {
    let app_msg: AppMessage = decode_one(&args.message).unwrap();
    let new_msg = AppMessage {
        text: app_msg.clone().text + " ping",
        timestamp: time(),
    };
    print(format!("[on_message] Received message"));
    send_app_message(args.client_key, new_msg)
}

fn send_app_message(client_key: ClientPublicKey, msg: AppMessage) {
    print(format!("Sending message"));
    if let Err(e) = ws_send(client_key, msg.candid_serialize()) {
        print(format!("Could not send message: {}", e));
    }
}

pub fn on_close(args: OnCloseCallbackArgs) {
    // remove client from the list of connected clients
    CLIENTS_CONNECTED.with(|clients_connected| {
        clients_connected.borrow_mut().remove(&args.client_key);

        print(format!(
            "[on_close] # clients connected: {}",
            clients_connected.borrow().len()
        ));
    });
}

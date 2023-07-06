use ic_cdk::export::candid::CandidType;
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, Serializer};

use crate::{sock::send_message_from_canister, WebsocketMessage, PublicKeySlice};

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct AppMessage {
    pub text: String,
}

pub fn ws_on_open(client_key: PublicKeySlice) {
    let msg = AppMessage {
        text: String::from("ping"),
    };
    ws_send_app_message(client_key, msg);
}

pub fn ws_on_message(content: WebsocketMessage) {
    let app_msg: AppMessage = from_slice(&content.message).unwrap();
    let new_msg = AppMessage {
        text: app_msg.text + " ping",
    };
    ws_send_app_message(content.client_key, new_msg)
}

pub fn ws_send_app_message(client_key: PublicKeySlice, msg: AppMessage) {
    let mut msg_cbor = vec![];
    let mut serializer = Serializer::new(&mut msg_cbor);
    serializer.self_describe().unwrap();
    msg.serialize(&mut serializer).unwrap();

    send_message_from_canister(client_key, msg_cbor);
}

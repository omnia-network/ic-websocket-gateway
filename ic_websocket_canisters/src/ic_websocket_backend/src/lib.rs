use ic_cdk_macros::*;

use canister::{on_close, on_message, on_open};
use sock::{wipe, WsCloseResult, WsMessageResult, WsOpenResult};
use sock::{CertMessages, GatewayMessage, PublicKeySlice};

mod canister;
mod sock;

#[init]
fn init() {
    sock::init(on_open, on_message, on_close)
}

#[post_upgrade]
fn post_upgrade() {
    sock::init(on_open, on_message, on_close)
}

// method called by the client SDK when instantiating a new IcWebSocket
#[update]
fn ws_register(client_key: PublicKeySlice) {
    sock::ws_register(client_key);
}

// method called by the WS Gateway after receiving FirstMessage from the client
#[update]
fn ws_open(msg: Vec<u8>, sig: Vec<u8>) -> WsOpenResult {
    sock::ws_open(msg, sig)
}

// method called by the Ws Gateway when closing the IcWebSocket connection
#[update]
fn ws_close(client_key: Vec<u8>) -> WsCloseResult {
    sock::ws_close(client_key)
}

// method called by the WS Gateway to send a message of type GatewayMessage to the canister
#[update]
fn ws_message(msg: GatewayMessage) -> WsMessageResult {
    sock::ws_message(msg)
}

// method called by the WS Gateway to get messages for all the clients it serves
#[query]
fn ws_get_messages(nonce: u64) -> CertMessages {
    sock::ws_get_messages(nonce)
}

// debug method used to wipe all data in the canister
#[update]
fn ws_wipe() {
    wipe();
}

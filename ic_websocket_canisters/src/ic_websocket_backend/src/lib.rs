use ic_cdk_macros::*;

use canister::{on_close, on_message, on_open};
use sock::wipe;
use sock::{CertMessages, PublicKeySlice};

pub mod canister;
pub mod sock;

#[init]
fn init() {
    sock::init(on_open, on_message, on_close)
}

#[post_upgrade]
fn post_upgrade() {
    sock::init(on_open, on_message, on_close)
}

#[update]
fn ws_register(client_key: PublicKeySlice) {
    sock::ws_register(client_key);
}

// Open the websocket connection.
#[update]
fn ws_open(msg: Vec<u8>, sig: Vec<u8>) -> bool {
    sock::ws_open(msg, sig)
}

// Close the websocket connection.
#[update]
fn ws_close(client_key: Vec<u8>) {
    sock::ws_close(client_key);
}

// Gateway calls this method to pass on the message from the client to the canister.
#[update]
fn ws_message(msg: Vec<u8>) -> bool {
    sock::ws_message(msg)
}

// Gateway polls this method to get messages for all the clients it serves.
#[query]
fn ws_get_messages(nonce: u64) -> CertMessages {
    sock::ws_get_messages(nonce)
}

// Debug method. Wipes all data in the canister.
#[update]
fn ws_wipe() {
    wipe();
}

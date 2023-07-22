use candid::Principal;
use ic_cdk_macros::*;

use canister::{on_close, on_message, on_open};
use sock::wipe;
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
// client submits its newly generated public key
#[update]
fn ws_register(client_key: PublicKeySlice) {
    sock::ws_register(client_key);
}

// method called by the WS Gateway after receiving FirstMessage from the client
// WS Gateway relays FirstMessage sent by the client together with its signature
// to prove that FirstMessage is actually coming from the same client that registered its public key
// beforehand by calling ws_register()
#[update]
fn ws_open(msg: Vec<u8>, sig: Vec<u8>) -> Result<(Vec<u8>, Principal), String> {
    sock::ws_open(msg, sig)
}

// Close the websocket connection.
#[update]
fn ws_close(client_key: Vec<u8>) {
    sock::ws_close(client_key);
}

// Gateway calls this method to pass on the message from the client to the canister.
#[update]
fn ws_message(msg: GatewayMessage) -> bool {
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

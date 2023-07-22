use ic_cdk_macros::*;

use canister::{on_close, on_message, on_open};
use sock::{wipe, WsOpenResult, WsMessageResult, WsCloseResult};
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
fn ws_open(msg: Vec<u8>, sig: Vec<u8>) -> WsOpenResult {
    sock::ws_open(msg, sig)
}

// Close the websocket connection.
#[update]
fn ws_close(client_key: Vec<u8>) -> WsCloseResult {
    sock::ws_close(client_key)
}

// method called by the WS Gateway to send a message of type GatewayMessage to the canister
// GatewayMessage has two variants:
// - IcWebSocketEstablished: message sent from WS Gateway to the canister to notify it about the
//                           establishment of the IcWebSocketConnection
// - RelayedFromClient: message sent from the client to the WS Gateway (via WebSocket) and
//                      relayed to the canister by the WS Gateway
#[update]
fn ws_message(msg: GatewayMessage) -> WsMessageResult {
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

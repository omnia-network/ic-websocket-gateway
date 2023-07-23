use ic_cdk_macros::*;

use canister::{on_close, on_message, on_open, GATEWAY_PRINCIPAL};
use sock::{
    CanisterWsCloseArguments, CanisterWsCloseResult, CanisterWsGetMessagesArguments,
    CanisterWsGetMessagesResult, CanisterWsMessageArguments, CanisterWsMessageResult,
    CanisterWsOpenArguments, CanisterWsOpenResult, CanisterWsRegisterArguments,
    CanisterWsRegisterResult,
};

mod canister;
mod sock;

#[init]
fn init() {
    sock::init(on_open, on_message, on_close, GATEWAY_PRINCIPAL)
}

#[post_upgrade]
fn post_upgrade() {
    sock::init(on_open, on_message, on_close, GATEWAY_PRINCIPAL)
}

// method called by the client SDK when instantiating a new IcWebSocket
#[update]
fn ws_register(args: CanisterWsRegisterArguments) -> CanisterWsRegisterResult {
    sock::ws_register(args)
}

// method called by the WS Gateway after receiving FirstMessage from the client
#[update]
fn ws_open(args: CanisterWsOpenArguments) -> CanisterWsOpenResult {
    sock::ws_open(args)
}

// method called by the Ws Gateway when closing the IcWebSocket connection
#[update]
fn ws_close(args: CanisterWsCloseArguments) -> CanisterWsCloseResult {
    sock::ws_close(args)
}

// method called by the WS Gateway to send a message of type GatewayMessage to the canister
#[update]
fn ws_message(args: CanisterWsMessageArguments) -> CanisterWsMessageResult {
    sock::ws_message(args)
}

// method called by the WS Gateway to get messages for all the clients it serves
#[query]
fn ws_get_messages(args: CanisterWsGetMessagesArguments) -> CanisterWsGetMessagesResult {
    sock::ws_get_messages(args)
}

// debug method used to wipe all data in the canister
#[update]
fn ws_wipe() {
    sock::wipe();
}

use ic_cdk_macros::*;
use std::{cell::RefCell, collections::HashSet};

use canister::{on_close, on_message, on_open, AppMessage};
use ic_websocket_cdk::{
    CanisterSendResult, CanisterWsCloseArguments, CanisterWsCloseResult,
    CanisterWsGetMessagesArguments, CanisterWsGetMessagesResult, CanisterWsMessageArguments,
    CanisterWsMessageResult, CanisterWsOpenArguments, CanisterWsOpenResult, ClientPrincipal,
    WsHandlers, WsInitParams,
};

mod canister;

thread_local! {
    /* flexible */ static CLIENTS_CONNECTED: RefCell<HashSet<ClientPrincipal>> = RefCell::new(HashSet::new());
}

#[init]
fn init() {
    let handlers = WsHandlers {
        on_open: Some(on_open),
        on_message: Some(on_message),
        on_close: Some(on_close),
    };

    let params = WsInitParams::new(handlers);

    ic_websocket_cdk::init(params)
}

#[post_upgrade]
fn post_upgrade() {
    init();
}

// method called by the WS Gateway after receiving open message from the client
#[update]
fn ws_open(args: CanisterWsOpenArguments) -> CanisterWsOpenResult {
    ic_websocket_cdk::ws_open(args)
}

// method called by the Ws Gateway when closing the IcWebSocket connection
#[update]
fn ws_close(args: CanisterWsCloseArguments) -> CanisterWsCloseResult {
    ic_websocket_cdk::ws_close(args)
}

// method called by the WS Gateway to send a message of type GatewayMessage to the canister
#[update]
fn ws_message(
    args: CanisterWsMessageArguments,
    msg_type: Option<AppMessage>,
) -> CanisterWsMessageResult {
    ic_websocket_cdk::ws_message(args, msg_type)
}

// method called by the WS Gateway to get messages for all the clients it serves
#[query]
fn ws_get_messages(args: CanisterWsGetMessagesArguments) -> CanisterWsGetMessagesResult {
    ic_websocket_cdk::ws_get_messages(args)
}

//// Debug/tests methods
// send a message to the client, usually called by the canister itself
#[update]
fn send(client_key: ClientPrincipal, msg_bytes: Vec<u8>) -> CanisterSendResult {
    ic_websocket_cdk::send(client_key, msg_bytes)
}

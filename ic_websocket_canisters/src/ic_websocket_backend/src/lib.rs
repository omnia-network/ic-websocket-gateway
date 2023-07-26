use ic_cdk_macros::*;

use canister::{on_close, on_message, on_open, AppMessage, GATEWAY_PRINCIPAL};
use sock::{
    CanisterWsCloseArguments, CanisterWsCloseResult, CanisterWsGetMessagesArguments,
    CanisterWsGetMessagesResult, CanisterWsMessageArguments, CanisterWsMessageResult,
    CanisterWsOpenArguments, CanisterWsOpenResult, CanisterWsRegisterArguments,
    CanisterWsRegisterResult, CanisterWsSendResult, ClientPublicKey, WsHandlers,
};

mod canister;
mod sock;

#[init]
fn init(gateway_principal: Option<String>) {
    let handlers = WsHandlers {
        on_open: Some(on_open),
        on_message: Some(on_message),
        on_close: Some(on_close),
    };

    if let Some(gateway_principal) = gateway_principal {
        sock::init(handlers, &gateway_principal)
    } else {
        sock::init(handlers, GATEWAY_PRINCIPAL)
    }
}

#[post_upgrade]
fn post_upgrade(gateway_principal: Option<String>) {
    let handlers = WsHandlers {
        on_open: Some(on_open),
        on_message: Some(on_message),
        on_close: Some(on_close),
    };

    if let Some(gateway_principal) = gateway_principal {
        sock::init(handlers, &gateway_principal)
    } else {
        sock::init(handlers, GATEWAY_PRINCIPAL)
    }
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

//// Debug/tests methods
// wipe all websocket data in the canister
#[update]
fn ws_wipe() {
    sock::wipe();
}

// send a message to the client, usually called by the canister itself
#[update]
fn ws_send(client_key: ClientPublicKey, msg: AppMessage) -> CanisterWsSendResult {
    sock::ws_send(client_key, msg)
}

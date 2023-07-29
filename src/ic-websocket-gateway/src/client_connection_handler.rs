use std::sync::Arc;

use futures_util::{SinkExt, StreamExt, TryStreamExt};
use ic_agent::Agent;
use serde_cbor::to_vec;
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, UnboundedSender},
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error, Message},
};

use crate::{
    canister_methods::{self, CanisterWsOpenResultValue},
    gateway_server::GatewaySession,
};

/// possible states of the WebSocket connection:
/// - established
/// - closed
/// - error
#[derive(Debug, Clone)]
pub enum WsConnectionState {
    /// WebSocket connection between client and WS Gateway established
    // does not imply that the IC WebSocket connection has also been established
    ConnectionEstablished(GatewaySession),
    /// WebSocket connection between client and WS Gateway closed
    ConnectionClosed(u64),
    /// error while handling WebSocket connection
    ConnectionError(IcWsError),
}

/// possible errors that can occur during a IC WebSocket connection
#[derive(Debug, Clone)]
pub enum IcWsError {
    /// error due to the client not following the IC WS initialization protocol
    InitializationError(String),
    /// WebSocket error
    WsError(String),
}

pub async fn handle_incoming_requests(
    agent: Arc<Agent>,
    listener: Arc<TcpListener>,
    client_connection_handler_tx: UnboundedSender<WsConnectionState>,
) {
    let mut next_client_id = 0; // needed to know which gateway_session to delete in case of error or WS closed
    while let Ok((stream, _client_addr)) = listener.accept().await {
        let agent_cl = Arc::clone(&agent);
        let client_connection_handler_tx_cl = client_connection_handler_tx.clone();
        // spawn a connection handler task for each incoming connection
        let current_client_id = next_client_id;
        tokio::spawn(async move {
            println!("\nNew client id: {}", current_client_id);
            handle_client_connection(
                current_client_id,
                &*agent_cl,
                stream,
                client_connection_handler_tx_cl,
            )
            .await;
        });
        next_client_id += 1;
    }
}

pub async fn handle_client_connection(
    client_id: u64,
    agent: &Agent,
    stream: TcpStream,
    client_connection_handler_tx: UnboundedSender<WsConnectionState>,
) {
    match accept_async(stream).await {
        Ok(ws_stream) => {
            let (mut ws_write, mut ws_read) = ws_stream.split();
            let mut is_first_message = true;
            // create channel which will be used to send messages from the canister poller directly to this client
            let (message_for_client_tx, mut message_for_client_rx) = mpsc::unbounded_channel();
            loop {
                select! {
                    // wait for incoming message from client
                    msg_res = ws_read.try_next() => {
                        match msg_res {
                            Ok(Some(message)) => {
                                // check if the WebSocket connection is closed
                                if message.is_close() {
                                    // let the main task know that it should remove the client's session from the WS Gateway state
                                    client_connection_handler_tx.send(
                                        WsConnectionState::ConnectionClosed(client_id)
                                    ).expect("channel should be open on the main thread");
                                    // break from the loop so that the connection handler task can terminate
                                    break;
                                }
                                // check if it is the first message being sent by the client via WebSocket
                                if is_first_message {
                                    // check if client followed the IC WebSocket connection establishment protocol
                                    match canister_methods::check_canister_init(agent, message.clone()).await {
                                        Ok(CanisterWsOpenResultValue {
                                            client_key,
                                            canister_id,
                                            // nonce is used by a new poller to know which message nonce to start polling from (if needed)
                                            // the nonce is obtained from the canister every time a client connects and the ws_open is called by the WS Gateway
                                            nonce,
                                        }) => {
                                            // let the client know that the IC WS connection is setup correctly
                                            ws_write.send(Message::Text("1".to_string())).await.expect("WS connection should be open");

                                            // create a new sender side of the channel which will be used to send canister messages
                                            // from the poller task directly to the client's connection handler task
                                            let message_for_client_tx_cl = message_for_client_tx.clone();
                                            // instantiate a new GatewaySession and send it to the main thread
                                            client_connection_handler_tx.send(
                                                WsConnectionState::ConnectionEstablished(
                                                    GatewaySession::new(
                                                        client_id,
                                                        client_key,
                                                        canister_id,
                                                        message_for_client_tx_cl,
                                                        nonce,
                                                    ),
                                                )
                                            ).expect("channel should be open on the main thread");
                                        },
                                        Err(e) => {
                                            // tell the client that the setup of the IC WS connection failed
                                            ws_write.send(Message::Text("0".to_string())).await.expect("WS connection should be open");
                                            // if this branch is executed, the Ok branch is never been executed, hence the WS Gateway state
                                            // does not contain any session for this client and therefore there is no cleanup needed
                                            client_connection_handler_tx
                                                .send(WsConnectionState::ConnectionError(IcWsError::InitializationError(e)))
                                                .expect("channel should be open on the main thread");
                                            // break from the loop so that the connection handler task can terminate
                                            break;
                                        }
                                    };
                                    // makes sure that this branch is executed at most once
                                    is_first_message = false;
                                }
                                else {
                                    // TODO: handle incoming message from client
                                    // println!("Client sent message: {:?}", message);
                                }
                            }
                            // in this case, client's session should have been cleaned up on the WS Gateway state already
                            // once the connection handler received Message::Close
                            // therefore, no additional cleanup is needed
                            Ok(None) => {
                                client_connection_handler_tx
                                    .send(WsConnectionState::ConnectionError(IcWsError::WsError(Error::AlreadyClosed.to_string())))
                                    .expect("channel should be open on the main thread");
                                // break from the loop so that the connection handler task can terminate
                                break;
                            },
                            // the client's still needs to be cleaned up so it is necessary to return the client id
                            Err(_) => {
                                // let the main task know that it should remove the client's session from the WS Gateway state
                                client_connection_handler_tx.send(
                                    WsConnectionState::ConnectionClosed(client_id)
                                ).expect("channel should be open on the main thread");
                                // break from the loop so that the connection handler task can terminate
                                break;
                            }
                        }
                    }
                    // wait for canister message to send to client
                    Some(message) = message_for_client_rx.recv() => {
                        // relay canister message to client, cbor encoded
                        ws_write.send(Message::Binary(to_vec(&message).unwrap())).await.expect("WS connection should be open");
                    }
                }
            }
        },
        // no cleanup needed on the WS Gateway has the client's session has never been created
        Err(e) => {
            client_connection_handler_tx
                .send(WsConnectionState::ConnectionError(IcWsError::WsError(
                    e.to_string(),
                )))
                .expect("channel should be open on the main thread");
        },
    }
}

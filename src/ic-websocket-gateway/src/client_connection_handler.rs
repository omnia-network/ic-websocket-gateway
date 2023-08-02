use std::sync::Arc;

use futures_util::{stream::SplitSink, SinkExt, StreamExt, TryStreamExt};
use ic_agent::Agent;
use serde_cbor::to_vec;
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Sender},
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error, Message},
    WebSocketStream,
};
use tracing::{error, info, span, warn, Instrument, Level};

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

pub struct WsConnectionsHandler {
    // listener of incoming TCP connections
    listener: TcpListener,
    agent: Arc<Agent>,
    client_connection_handler_tx: Sender<WsConnectionState>,
    // needed to know which gateway_session to delete in case of error or WS closed
    next_client_id: u64,
}

impl WsConnectionsHandler {
    pub async fn new(
        gateway_address: &str,
        agent: Arc<Agent>,
        client_connection_handler_tx: Sender<WsConnectionState>,
    ) -> Self {
        let listener = TcpListener::bind(&gateway_address)
            .await
            .expect("Can't listen");
        Self {
            listener,
            agent,
            client_connection_handler_tx,
            next_client_id: 0,
        }
    }

    pub async fn listen_for_incoming_requests(&mut self) {
        while let Ok((stream, client_addr)) = self.listener.accept().await {
            let agent_cl = Arc::clone(&self.agent);
            let client_connection_handler_tx_cl = self.client_connection_handler_tx.clone();
            // spawn a connection handler task for each incoming client connection
            let current_client_id = self.next_client_id;
            let span = span!(
                Level::INFO,
                "handle_client_connection",
                client_addr = ?client_addr,
                client_id = current_client_id
            );
            tokio::spawn(
                async move {
                    let client_connection_handler = ClientConnectionHandler::new(
                        current_client_id,
                        agent_cl,
                        client_connection_handler_tx_cl,
                    );
                    info!("Spawned new connection handler");
                    client_connection_handler.handle_stream(stream).await;
                }
                .instrument(span),
            );
            self.next_client_id += 1;
        }
    }
}

struct ClientConnectionHandler {
    id: u64,
    agent: Arc<Agent>,
    client_connection_handler_tx: Sender<WsConnectionState>,
}
impl ClientConnectionHandler {
    pub fn new(
        id: u64,
        agent: Arc<Agent>,
        client_connection_handler_tx: Sender<WsConnectionState>,
    ) -> Self {
        Self {
            id,
            agent,
            client_connection_handler_tx,
        }
    }
    pub async fn handle_stream(&self, stream: TcpStream) {
        match accept_async(stream).await {
            Ok(ws_stream) => {
                info!("Accepted WebSocket connection");
                let (mut ws_write, mut ws_read) = ws_stream.split();
                let mut is_first_message = true;
                // create channel which will be used to send messages from the canister poller directly to this client
                let (message_for_client_tx, mut message_for_client_rx) = mpsc::channel(100);
                loop {
                    select! {
                        // wait for incoming message from client
                        msg_res = ws_read.try_next() => {
                            match msg_res {
                                Ok(Some(message)) => {
                                    // check if the WebSocket connection is closed
                                    if message.is_close() {
                                        // let the main task know that it should remove the client's session from the WS Gateway state
                                        self.send_connection_state_to_clients_manager(WsConnectionState::ConnectionClosed(self.id)).await;
                                        info!("Client closed the Websocket connection: {:?}", message);
                                        // break from the loop so that the connection handler task can terminate
                                        break;
                                    }
                                    // check if it is the first message being sent by the client via WebSocket
                                    if is_first_message {
                                        // check if client followed the IC WebSocket connection establishment protocol
                                        match canister_methods::check_canister_init(&self.agent, message.clone()).await {
                                            Ok(CanisterWsOpenResultValue {
                                                client_key,
                                                canister_id,
                                                // nonce is used by a new poller to know which message nonce to start polling from (if needed)
                                                // the nonce is obtained from the canister every time a client connects and the ws_open is called by the WS Gateway
                                                nonce,
                                            }) => {
                                                info!("Client established IC WebSocket connection");
                                                // let the client know that the IC WS connection is setup correctly
                                                send_ws_message_to_client(&mut ws_write, Message::Text("1".to_string())).await;

                                                // create a new sender side of the channel which will be used to send canister messages
                                                // from the poller task directly to the client's connection handler task
                                                let message_for_client_tx_cl = message_for_client_tx.clone();
                                                // instantiate a new GatewaySession and send it to the main thread
                                                self.send_connection_state_to_clients_manager(
                                                    WsConnectionState::ConnectionEstablished(
                                                        GatewaySession::new(
                                                            self.id,
                                                            client_key,
                                                            canister_id,
                                                            message_for_client_tx_cl,
                                                            nonce,
                                                        ),
                                                    )
                                                ).await;
                                            },
                                            Err(e) => {
                                                info!("Client did not follow IC WebSocket establishment protocol: {:?}", e);
                                                // tell the client that the setup of the IC WS connection failed
                                                send_ws_message_to_client(&mut ws_write, Message::Text("0".to_string())).await;
                                                // if this branch is executed, the Ok branch is never been executed, hence the WS Gateway state
                                                // does not contain any session for this client and therefore there is no cleanup needed
                                                self.send_connection_state_to_clients_manager(
                                                    WsConnectionState::ConnectionError(IcWsError::InitializationError(e))
                                                ).await;
                                                // break from the loop so that the connection handler task can terminate
                                                break;
                                            }
                                        };
                                        // makes sure that this branch is executed at most once
                                        is_first_message = false;
                                    }
                                    else {
                                        warn!("Client sent a message via WebSocket connection: {:?}", message);
                                        // TODO: handle incoming message from client
                                    }
                                }
                                // in this case, client's session should have been cleaned up on the WS Gateway state already
                                // once the connection handler received Message::Close
                                // therefore, no additional cleanup is needed
                                Ok(None) => {

                                    self.send_connection_state_to_clients_manager(
                                        WsConnectionState::ConnectionError(IcWsError::WsError(Error::AlreadyClosed.to_string()))
                                    ).await;
                                    warn!("Client WebSocket connection already closed");
                                    // break from the loop so that the connection handler task can terminate
                                    break;
                                },
                                // the client's still needs to be cleaned up so it is necessary to return the client id
                                Err(e) => {
                                    // let the main task know that it should remove the client's session from the WS Gateway state
                                    self.send_connection_state_to_clients_manager(
                                        WsConnectionState::ConnectionClosed(self.id)
                                    )
                                    .await;
                                    info!("Client WebSocket connection error: {:?}", e);
                                    // break from the loop so that the connection handler task can terminate
                                    break;
                                }
                            }
                        }
                        // wait for canister message to send to client
                        Some(canister_message) = message_for_client_rx.recv() => {
                            info!("Sending message with key: {:?} to client", canister_message.key);
                            // relay canister message to client, cbor encoded
                            match to_vec(&canister_message) {
                                Ok(bytes) => {
                                    send_ws_message_to_client(&mut ws_write, Message::Binary(bytes)).await;
                                    info!("Message with key: {:?} sent to client", canister_message.key);
                                },
                                Err(e) => error!("Could not serialize canister message. Error: {:?}", e)
                            }
                        }
                    }
                }
                info!("Terminating client connection handler task");
            },
            // no cleanup needed on the WS Gateway has the client's session has never been created
            Err(e) => {
                info!("Refused WebSocket connection {:?}", e);
                self.send_connection_state_to_clients_manager(WsConnectionState::ConnectionError(
                    IcWsError::WsError(e.to_string()),
                ))
                .await;
            },
        }
    }

    async fn send_connection_state_to_clients_manager(&self, connection_state: WsConnectionState) {
        if let Err(e) = self
            .client_connection_handler_tx
            .send(connection_state)
            .await
        {
            error!(
                "Receiver has been dropped on the clients connection manager's side. Error: {:?}",
                e
            );
        }
    }
}

async fn send_ws_message_to_client(
    ws_write: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
    message: Message,
) {
    if let Err(e) = ws_write.send(message).await {
        // TODO: graceful shutdown fo client task
        error!("Could not send message to client: {:?}", e);
    }
}

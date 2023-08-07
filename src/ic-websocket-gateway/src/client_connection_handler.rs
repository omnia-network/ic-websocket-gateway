use crate::{
    canister_methods::{self, CanisterWsOpenResultValue},
    canister_poller::CertifiedMessage,
    gateway_server::GatewaySession,
};
use futures_util::{stream::SplitSink, SinkExt, StreamExt, TryStreamExt};
use ic_agent::Agent;
use native_tls::Identity;
use serde_cbor::to_vec;
use std::{fs, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Sender},
};
use tokio_native_tls::{TlsAcceptor, TlsStream};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, span, warn, Instrument, Level, Span};

/// possible states of the WebSocket connection:
/// - established
/// - closed
/// - error
#[derive(Debug, Clone)]
pub enum WsConnectionState {
    /// WebSocket connection between client and WS Gateway established
    // does not imply that the IC WebSocket connection has also been established
    Established(GatewaySession),
    /// WebSocket connection between client and WS Gateway closed
    Closed(u64),
    /// error while handling WebSocket connection
    Error(IcWsError),
}

/// possible errors that can occur during a IC WebSocket connection
#[derive(Debug, Clone)]
pub enum IcWsError {
    /// error due to the client not following the IC WS initialization protocol
    Initialization(String),
    /// WebSocket error
    WebSocket(String),
}

/// Possible TCP streams.
enum CustomStream {
    Tcp(TcpStream),
    TcpWithTls(TlsStream<TcpStream>),
}

pub struct TlsConfig {
    pub certificate_pem_path: String,
    pub certificate_key_pem_path: String,
}

pub struct WsConnectionsHandler {
    // listener of incoming TCP connections
    listener: TcpListener,
    tls_acceptor: Option<TlsAcceptor>,
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
        tls_config: Option<TlsConfig>,
    ) -> Self {
        let listener = TcpListener::bind(&gateway_address)
            .await
            .expect("Can't listen");
        let mut tls_acceptor = None;
        if let Some(tls_config) = tls_config {
            let chain = fs::read(tls_config.certificate_pem_path).expect("Can't read certificate");
            let privkey =
                fs::read(tls_config.certificate_key_pem_path).expect("Can't read private key");
            let tls_identity =
                Identity::from_pkcs8(&chain, &privkey).expect("Can't create a TLS identity");
            let acceptor = TlsAcceptor::from(
                native_tls::TlsAcceptor::builder(tls_identity)
                    .build()
                    .expect("Can't create a TLS acceptor from the TLS identity"),
            );
            tls_acceptor = Some(acceptor);
            info!("TLS enabled");
        } else {
            info!("TLS disabled");
        }
        Self {
            listener,
            tls_acceptor,
            agent,
            client_connection_handler_tx,
            next_client_id: 0,
        }
    }

    pub async fn listen_for_incoming_requests(&mut self, parent_token: CancellationToken) {
        // needed to ensure that we stop listening for incoming requests before we start shutting down the connections
        let child_token = CancellationToken::new();

        let wait_for_cancellation = parent_token.cancelled();
        tokio::pin!(wait_for_cancellation);
        loop {
            select! {
                Ok((stream, client_addr)) = self.listener.accept() => {
                    let current_client_id = self.next_client_id;
                    let span = span!(
                        Level::INFO,
                        "handle_client_connection",
                        client_addr = ?client_addr,
                        client_id = current_client_id
                    );
                    let _guard = span.enter();

                    let stream = match self.tls_acceptor {
                        Some(ref acceptor) => {
                            let tls_stream = acceptor.accept(stream).await;
                            match tls_stream {
                                Ok(tls_stream) => {
                                    debug!("TLS handshake successful");
                                    CustomStream::TcpWithTls(tls_stream)
                                },
                                Err(e) => {
                                    error!("TLS handshake failed: {:?}", e);
                                    continue;
                                },
                            }
                        },
                        None => CustomStream::Tcp(stream),
                    };
                    self.start_connection_handler(stream, current_client_id, child_token.clone(), span.clone());

                    self.next_client_id += 1;
                },
                _ = &mut wait_for_cancellation => {
                    child_token.cancel();
                    warn!("Stopped listening for incoming requests");
                    break;
                }

            }
        }
    }

    fn start_connection_handler(
        &self,
        stream: CustomStream,
        client_id: u64,
        token: CancellationToken,
        span: Span,
    ) {
        let agent = Arc::clone(&self.agent);
        let client_connection_handler_tx = self.client_connection_handler_tx.clone();
        // spawn a connection handler task for each incoming client connection
        tokio::spawn(
            async move {
                let client_connection_handler = ClientConnectionHandler::new(
                    client_id,
                    agent,
                    client_connection_handler_tx,
                    token,
                );
                info!("Spawned new connection handler");
                match stream {
                    CustomStream::Tcp(stream) => {
                        client_connection_handler.handle_stream(stream).await
                    },
                    CustomStream::TcpWithTls(stream) => {
                        client_connection_handler.handle_stream(stream).await
                    },
                }
                info!("Terminated client connection handler task");
            }
            .instrument(span),
        );
    }
}

struct ClientConnectionHandler {
    id: u64,
    agent: Arc<Agent>,
    client_connection_handler_tx: Sender<WsConnectionState>,
    token: CancellationToken,
}

impl ClientConnectionHandler {
    pub fn new(
        id: u64,
        agent: Arc<Agent>,
        client_connection_handler_tx: Sender<WsConnectionState>,
        token: CancellationToken,
    ) -> Self {
        Self {
            id,
            agent,
            client_connection_handler_tx,
            token,
        }
    }

    pub async fn handle_stream<S: AsyncRead + AsyncWrite + Unpin>(&self, stream: S) {
        match accept_async(stream).await {
            Ok(ws_stream) => {
                info!("Accepted WebSocket connection");
                let (mut ws_write, mut ws_read) = ws_stream.split();

                // [client connection handler task]        [poller task]
                // message_for_client_rx            <----- message_for_client_tx

                // channel used by the poller task to send canister messages from the directly to this client connection handler task
                // which will then forward it to the client via the WebSocket connection
                let (message_for_client_tx, mut message_for_client_rx) = mpsc::channel(100);

                // [client connection handler task]        [main task]
                // terminate_client_handler_rx      <----- terminate_client_handler_tx

                // channel used by the main task to to let this client connection handler task know that it should terminate
                // this is used when the poller task detects an error from the canister and informs the main task that all client
                // connections to the respective canister should be closed
                let (terminate_client_handler_tx, mut terminate_client_handler_rx) =
                    mpsc::channel(1);

                let wait_for_cancellation = self.token.cancelled();
                tokio::pin!(wait_for_cancellation);
                let mut is_first_message = true;
                loop {
                    select! {
                        // wait for incoming message from client
                        ws_message = ws_read.try_next() => {
                            if let Err(e) = self.handle_incoming_ws_message(
                                ws_message,
                                is_first_message,
                                &mut ws_write,
                                message_for_client_tx.clone(),
                                terminate_client_handler_tx.clone()
                            ).await {
                                // break from the loop so that the connection handler task can terminate
                                warn!(e);
                                break;
                            }
                            else {
                                is_first_message = false;
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
                        },
                        // waits for the token to be cancelled
                        _ = &mut wait_for_cancellation => {
                            self.send_connection_state_to_clients_manager(
                                WsConnectionState::Closed(self.id)
                            )
                            .await;
                            // close the WebSocket connection
                            ws_write.close().await.unwrap();
                            warn!("Terminating client connection handler task");
                            break;
                        },
                        // waits for the main task to signal that the connection should be closed
                        _ = terminate_client_handler_rx.recv() => {
                            // close the WebSocket connection
                            ws_write.close().await.unwrap();
                            error!("Terminating client connection handler task due to CDK error");
                            break;
                        }
                    }
                }
            },
            // no cleanup needed on the WS Gateway has the client's session has never been created
            Err(e) => {
                info!("Refused WebSocket connection {:?}", e);
                self.send_connection_state_to_clients_manager(WsConnectionState::Error(
                    IcWsError::WebSocket(e.to_string()),
                ))
                .await;
            },
        }
    }

    async fn handle_incoming_ws_message<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        ws_message: Result<Option<Message>, Error>,
        is_first_message: bool,
        mut ws_write: &mut SplitSink<WebSocketStream<S>, Message>,
        message_for_client_tx: Sender<CertifiedMessage>,
        terminate_client_handler_tx: Sender<bool>,
    ) -> Result<(), String> {
        match ws_message {
            // handle message sent from client via WebSocket
            Ok(Some(message)) => {
                // check if the WebSocket connection is closed
                if message.is_close() {
                    // let the main task know that it should remove the client's session from the WS Gateway state
                    self.send_connection_state_to_clients_manager(WsConnectionState::Closed(
                        self.id,
                    ))
                    .await;
                    // break from the loop so that the connection handler task can terminate
                    return Err(String::from("Client closed the Websocket connection"));
                }
                // check if it is the first message being sent by the client via WebSocket
                if is_first_message {
                    // check if client followed the IC WebSocket connection establishment protocol
                    match canister_methods::check_canister_init(&self.agent, message.clone()).await
                    {
                        Ok(CanisterWsOpenResultValue {
                            client_key,
                            canister_id,
                            // nonce is used by a new poller to know which message nonce to start polling from (if needed)
                            // the nonce is obtained from the canister every time a client connects and the ws_open is called by the WS Gateway
                            nonce,
                        }) => {
                            // prevent adding a new client to the gateway state while shutting down
                            // neeeded because wait_for_cancellation might be ready while handle_incoming_ws_message
                            // is already executing
                            if !self.token.is_cancelled() {
                                info!("Client established IC WebSocket connection");
                                // let the client know that the IC WS connection is setup correctly
                                send_ws_message_to_client(
                                    &mut ws_write,
                                    Message::Text("1".to_string()),
                                )
                                .await;

                                // instantiate a new GatewaySession and send it to the main thread
                                self.send_connection_state_to_clients_manager(
                                    WsConnectionState::Established(GatewaySession::new(
                                        self.id,
                                        client_key,
                                        canister_id,
                                        message_for_client_tx,
                                        terminate_client_handler_tx,
                                        nonce,
                                    )),
                                )
                                .await;
                                Ok(())
                            } else {
                                Err(String::from("Preventing client connection handler task to establish new WS connection"))
                            }
                        },
                        Err(e) => {
                            // tell the client that the setup of the IC WS connection failed
                            send_ws_message_to_client(
                                &mut ws_write,
                                Message::Text("0".to_string()),
                            )
                            .await;
                            // if this branch is executed, the Ok branch is never been executed, hence the WS Gateway state
                            // does not contain any session for this client and therefore there is no cleanup needed
                            self.send_connection_state_to_clients_manager(
                                WsConnectionState::Error(IcWsError::Initialization(e.clone())),
                            )
                            .await;
                            // break from the loop so that the connection handler task can terminate
                            Err(format!(
                                "Client did not follow IC WebSocket establishment protocol: {:?}",
                                e
                            ))
                        },
                    }
                } else {
                    warn!(
                        "Client sent a message via WebSocket connection: {:?}",
                        message
                    );
                    // TODO: handle incoming message from client
                    Ok(())
                }
            },
            // in this case, client's session should have been cleaned up on the WS Gateway state already
            // once the connection handler received Message::Close
            // therefore, no additional cleanup is needed
            Ok(None) => {
                self.send_connection_state_to_clients_manager(WsConnectionState::Error(
                    IcWsError::WebSocket(Error::AlreadyClosed.to_string()),
                ))
                .await;
                Err(String::from("Client WebSocket connection already closed"))
            },
            // the client's still needs to be cleaned up so it is necessary to return the client id
            Err(e) => {
                // let the main task know that it should remove the client's session from the WS Gateway state
                self.send_connection_state_to_clients_manager(WsConnectionState::Closed(self.id))
                    .await;
                Err(format!("Client WebSocket connection error: {:?}", e))
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

async fn send_ws_message_to_client<S: AsyncRead + AsyncWrite + Unpin>(
    ws_write: &mut SplitSink<WebSocketStream<S>, Message>,
    message: Message,
) {
    if let Err(e) = ws_write.send(message).await {
        // TODO: graceful shutdown fo client task
        error!("Could not send message to client: {:?}", e);
    }
}

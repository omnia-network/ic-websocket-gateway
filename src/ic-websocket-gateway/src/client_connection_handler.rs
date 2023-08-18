use crate::{
    canister_methods::{self, CanisterWsOpenResultValue},
    canister_poller::CertifiedMessage,
    gateway_server::GatewaySession,
    metrics_analyzer::{Deltas, Metrics, MetricsReference, TimeableEvent},
};

use futures_util::{stream::SplitSink, SinkExt, StreamExt, TryStreamExt};
use ic_agent::Agent;
use serde_cbor::to_vec;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    select,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

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
    /// IC WS method not implemented yet
    NotImplemented(String),
}

#[derive(Debug)]
struct ConnectionSetupMetrics {
    reference: MetricsReference,
    accepted_ws_connection: TimeableEvent,
    received_first_message: TimeableEvent,
    validated_first_message: TimeableEvent,
    established_ws_connection: TimeableEvent,
}

impl ConnectionSetupMetrics {
    fn new(id: u64) -> Self {
        let reference = MetricsReference::ClientId(id);
        Self {
            reference,
            accepted_ws_connection: TimeableEvent::default(),
            received_first_message: TimeableEvent::default(),
            validated_first_message: TimeableEvent::default(),
            established_ws_connection: TimeableEvent::default(),
        }
    }
    fn set_accepted_ws_connection(&mut self) {
        self.accepted_ws_connection.set_now();
    }

    fn set_received_first_message(&mut self) {
        self.received_first_message.set_now();
    }

    fn set_validated_first_message(&mut self) {
        self.validated_first_message.set_now();
    }

    fn set_established_ws_connection(&mut self) {
        self.established_ws_connection.set_now();
    }
}

impl Metrics for ConnectionSetupMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.established_ws_connection
    }

    fn compute_deltas(&self) -> Option<Box<dyn Deltas + Send>> {
        let time_to_first_message = self
            .received_first_message
            .duration_since(&self.accepted_ws_connection)?;
        let time_to_validation = self
            .validated_first_message
            .duration_since(&self.received_first_message)?;
        let time_to_establishment = self
            .established_ws_connection
            .duration_since(&self.validated_first_message)?;
        let latency = self.compute_latency()?;

        Some(Box::new(ConnectionSetupDeltas::new(
            self.reference.clone(),
            time_to_first_message,
            time_to_validation,
            time_to_establishment,
            latency,
        )))
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.established_ws_connection
            .duration_since(&self.accepted_ws_connection)
    }
}

#[derive(Debug)]
struct ConnectionSetupDeltas {
    reference: MetricsReference,
    time_to_first_message: Duration,
    time_to_validation: Duration,
    time_to_establishment: Duration,
    latency: Duration,
}

impl ConnectionSetupDeltas {
    pub fn new(
        reference: MetricsReference,
        time_to_first_message: Duration,
        time_to_validation: Duration,
        time_to_establishment: Duration,
        latency: Duration,
    ) -> Self {
        Self {
            reference,
            time_to_first_message,
            time_to_validation,
            time_to_establishment,
            latency,
        }
    }
}

impl Deltas for ConnectionSetupDeltas {
    fn display(&self) {
        debug!(
            "\nreference: {:?}\ntime_to_first_message: {:?}\ntime_to_validation: {:?}\ntime_to_establishment: {:?}\nlatency: {:?}",
            self.reference, self.time_to_first_message, self.time_to_validation, self.time_to_establishment, self.latency
        );
    }

    fn get_reference(&self) -> &MetricsReference {
        &self.reference
    }

    fn get_latency(&self) -> Duration {
        self.latency
    }
}

pub struct ClientConnectionHandler {
    id: u64,
    agent: Arc<Agent>,
    client_connection_handler_tx: Sender<WsConnectionState>,
    metrics_channel_tx: Sender<Box<dyn Metrics + Send>>,
    token: CancellationToken,
}

impl ClientConnectionHandler {
    pub fn new(
        id: u64,
        agent: Arc<Agent>,
        client_connection_handler_tx: Sender<WsConnectionState>,
        metrics_channel_tx: Sender<Box<dyn Metrics + Send>>,
        token: CancellationToken,
    ) -> Self {
        Self {
            id,
            agent,
            client_connection_handler_tx,
            metrics_channel_tx,
            token,
        }
    }

    pub async fn handle_stream<S: AsyncRead + AsyncWrite + Unpin>(&self, stream: S) {
        match accept_async(stream).await {
            Ok(ws_stream) => {
                let mut connection_setup_metrics = Some(ConnectionSetupMetrics::new(self.id));
                connection_setup_metrics
                    .as_mut()
                    .unwrap()
                    .set_accepted_ws_connection();
                debug!("Accepted WebSocket connection");
                let (mut ws_write, mut ws_read) = ws_stream.split();

                // [client connection handler task]        [poller task]
                // message_for_client_rx            <----- message_for_client_tx

                // channel used by the poller task to send canister messages from the directly to this client connection handler task
                // which will then forward it to the client via the WebSocket connection
                let (message_for_client_tx, mut message_for_client_rx): (
                    Sender<Result<CertifiedMessage, String>>,
                    Receiver<Result<CertifiedMessage, String>>,
                ) = mpsc::channel(100);

                let wait_for_cancellation = self.token.cancelled();
                tokio::pin!(wait_for_cancellation);
                let mut is_first_message = true;
                loop {
                    select! {
                        // bias select! to check token cancellation first
                        // with 'biased', async functions are polled in the order in which they appear
                        biased;
                        // waits for the token to be cancelled
                        _ = &mut wait_for_cancellation => {
                            self.send_connection_state_to_clients_manager(
                                WsConnectionState::Closed(self.id)
                            )
                            .await;
                            // close the WebSocket connection
                            ws_write.close().await.unwrap();
                            debug!("Terminating client connection handler task");
                            break;
                        },
                        // wait for canister message to send to client
                        Some(poller_message) = message_for_client_rx.recv() => {
                            match poller_message {
                                // check if the poller task detected an error from the CDK
                                Ok(canister_message) => {
                                    debug!("Sending message with key: {:?} to client", canister_message.key);
                                    // relay canister message to client, cbor encoded
                                    match to_vec(&canister_message) {
                                        Ok(bytes) => {
                                            send_ws_message_to_client(&mut ws_write, Message::Binary(bytes)).await;
                                            debug!("Message with key: {:?} sent to client", canister_message.key);
                                        },
                                        Err(e) => error!("Could not serialize canister message. Error: {:?}", e)
                                    }
                                }
                                // the poller task terminates all the client connection tasks connected to that poller
                                Err(e) => {
                                    // close the WebSocket connection
                                    ws_write.close().await.unwrap();
                                    error!("Terminating client connection handler task. Error: {}", e);
                                    break;
                                }
                            }
                        },
                        // wait for incoming message from client
                        ws_message = ws_read.try_next() => {
                            match self.handle_incoming_ws_message(
                                ws_message,
                                is_first_message,
                                connection_setup_metrics,
                                &mut ws_write,
                                message_for_client_tx.clone(),
                            ).await {
                                // if the connection is successfully established, the following messages are not the first
                                WsConnectionState::Established(_) => is_first_message = false,
                                // if the client calls a method which is not implemented, we should only report it but we can keep the connection alive
                                WsConnectionState::Error(IcWsError::NotImplemented(e)) => warn!(e),
                                // if the client closes the WS connection, we terminate the connection handler task, without reporting any error
                                WsConnectionState::Closed(_) => break,
                                // in case of other errors, we report them and terminate the connection handler task
                                WsConnectionState::Error(e) => {
                                    warn!("{:?}", e);
                                    break;
                                }
                            }
                            // after the first time 'handle_incoming_ws_message' is called, we do not need to collect connection setup metrics anymore
                            // so we always pass None
                            connection_setup_metrics = None;
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
        mut connection_setup_metrics: Option<ConnectionSetupMetrics>,
        mut ws_write: &mut SplitSink<WebSocketStream<S>, Message>,
        message_for_client_tx: Sender<Result<CertifiedMessage, String>>,
    ) -> WsConnectionState {
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
                    return WsConnectionState::Closed(self.id);
                }
                // check if it is the first message being sent by the client via WebSocket
                if is_first_message {
                    connection_setup_metrics
                        .as_mut()
                        .expect("must have connection setup metrics for first message")
                        .set_received_first_message();
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
                            connection_setup_metrics
                                .as_mut()
                                .expect("must have connection setup metrics for first message")
                                .set_validated_first_message();
                            // prevent adding a new client to the gateway state while shutting down
                            // neeeded because wait_for_cancellation might be ready while handle_incoming_ws_message
                            // is already executing
                            if !self.token.is_cancelled() {
                                debug!("Client established IC WebSocket connection");
                                // let the client know that the IC WS connection is setup correctly
                                send_ws_message_to_client(
                                    &mut ws_write,
                                    Message::Text("1".to_string()),
                                )
                                .await;

                                let gateway_session = GatewaySession::new(
                                    self.id,
                                    client_key,
                                    canister_id,
                                    message_for_client_tx,
                                    nonce,
                                );
                                // instantiate a new GatewaySession and send it to the main thread
                                self.send_connection_state_to_clients_manager(
                                    WsConnectionState::Established(gateway_session.clone()),
                                )
                                .await;
                                connection_setup_metrics
                                    .as_mut()
                                    .expect("must have connection setup metrics for first message")
                                    .set_established_ws_connection();
                                self.metrics_channel_tx
                                    .send(Box::new(connection_setup_metrics.expect(
                                        "must have connection setup metrics for first message",
                                    )))
                                    .await
                                    .expect("analyzer's side of the channel dropped");
                                WsConnectionState::Established(gateway_session)
                            } else {
                                // if the gateway has already started the graceful shutdown, we have to prevent new clients from connecting
                                WsConnectionState::Error(IcWsError::WebSocket(String::from("Preventing client connection handler task to establish new WS connection")))
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
                                WsConnectionState::Error(IcWsError::Initialization(format!(
                                    "Client did not follow IC WebSocket establishment protocol: {:?}",
                                    e
                                ))),
                            )
                            .await;
                            WsConnectionState::Error(IcWsError::Initialization(format!(
                                "Client did not follow IC WebSocket establishment protocol: {:?}",
                                e
                            )))
                        },
                    }
                } else {
                    // TODO: handle incoming message from client
                    WsConnectionState::Error(IcWsError::NotImplemented(format!(
                        "Client sent a message via WebSocket connection: {:?}",
                        message
                    )))
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
                WsConnectionState::Error(IcWsError::WebSocket(String::from(
                    "Client WebSocket connection already closed",
                )))
            },
            // the client's still needs to be cleaned up so it is necessary to return the client id
            Err(e) => {
                // let the main task know that it should remove the client's session from the WS Gateway state
                self.send_connection_state_to_clients_manager(WsConnectionState::Closed(self.id))
                    .await;
                WsConnectionState::Error(IcWsError::WebSocket(format!(
                    "Client WebSocket connection error: {:?}",
                    e
                )))
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

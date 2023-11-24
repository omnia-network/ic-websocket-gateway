use crate::{
    canister_methods::{CanisterWsOpenArguments, ClientKey},
    canister_poller::IcWsConnectionUpdate,
    events_analyzer::{Events, EventsCollectionType, EventsReference, MessageReference},
    manager::ClientSession,
    messages_demux::get_nonce_from_message,
    metrics::client_connection_handler_metrics::{
        OutgoingCanisterMessageEvents, OutgoingCanisterMessageEventsMetrics,
        RequestConnectionSetupEvents, RequestConnectionSetupEventsMetrics,
    },
};
use candid::{decode_args, Principal};
use futures_util::{stream::SplitSink, SinkExt, StreamExt, TryStreamExt};
use ic_agent::{
    agent::{Envelope, EnvelopeContent},
    Agent, AgentError,
};
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, to_vec};
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    select,
    sync::{
        mpsc::{self, Receiver, Sender},
        RwLock,
    },
};
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, span, trace, warn, Instrument, Level, Span};

pub type ClientId = u64;

#[derive(Serialize, Deserialize)]
struct GatewayHandshakeMessage {
    gateway_principal: Principal,
}

/// Message sent by the client using the custom @dfinity/agent (via WS)
#[derive(Serialize, Deserialize)]
struct ClientRequest<'a> {
    /// Envelope of the signed request to the IC
    envelope: Envelope<'a>,
}

/// possible states of the IC WebSocket connection:
/// - setup
/// - requested
/// - closed
#[derive(Debug, Clone)]
pub enum IcWsConnectionState {
    /// IC WebSocket connection between client and gateway has been setup
    Setup(ClientSession),
    /// client requested IC WebSocket connection
    Requested(ClientKey),
    /// WebSocket connection between client and WS Gateway closed
    Closed((ClientKey, Principal, Span)),
}

/// possible errors that can occur during a IC WebSocket connection
#[derive(Debug, Clone)]
pub enum IcWsError {
    /// error due to the client not following the IC WS initialization protocol
    Initialization(String),
    /// WebSocket error
    WebSocket(String),
}

pub struct ClientConnectionHandler {
    id: ClientId,
    key: Option<ClientKey>,
    agent: Arc<Agent>,
    client_connection_handler_tx: Sender<IcWsConnectionState>,
    events_channel_tx: Sender<Box<dyn Events + Send>>,
    token: CancellationToken,
    // the client tells which canister it wants to connect to in the first envelope it sends via WS
    canister_id: Arc<RwLock<Option<Principal>>>,
}

impl ClientConnectionHandler {
    pub fn new(
        id: ClientId,
        agent: Arc<Agent>,
        client_connection_handler_tx: Sender<IcWsConnectionState>,
        events_channel_tx: Sender<Box<dyn Events + Send>>,
        token: CancellationToken,
    ) -> Self {
        Self {
            id,
            key: None,
            agent,
            client_connection_handler_tx,
            events_channel_tx,
            token,
            canister_id: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn handle_stream<S: AsyncRead + AsyncWrite + Unpin>(&mut self, stream: S) {
        match accept_async(stream).await {
            Ok(ws_stream) => {
                let mut request_connection_setup_events = RequestConnectionSetupEvents::new(
                    Some(EventsReference::ClientId(self.id)),
                    EventsCollectionType::NewClientConnection,
                    RequestConnectionSetupEventsMetrics::default(),
                );

                request_connection_setup_events
                    .metrics
                    .set_accepted_ws_connection();

                debug!("Accepted WebSocket connection");

                let (mut ws_write, mut ws_read) = ws_stream.split();

                // as soon as the WS connection with the client is established, send the gateway principal
                // needed because the client doesn't know the principal of the gateway it is connecting to but only it's IP
                // however, the client has to tell the canister CDK which principal is authorized to poll its updates from the canister queue,
                // the returned principal will be included by the client in the first envelope it sends via WS

                let handshake_message = GatewayHandshakeMessage {
                    gateway_principal: self.agent.get_principal().expect("Principal should be set"),
                };

                let ic_websocket_setup_span =
                    span!(parent: &Span::current(), Level::DEBUG, "ic_websocket_setup");
                if let Err(e) = send_ws_message_to_client(
                    &mut ws_write,
                    Message::Binary(
                        serialize(handshake_message).expect("Principal should be serializable"),
                    ),
                )
                .await
                {
                    ic_websocket_setup_span.in_scope(|| {
                        error!("Error sending handshake message to client: {:?}", e);
                    });
                }

                // [client connection handler task]        [poller task]
                // message_for_client_rx            <----- message_for_client_tx

                // channel used by the poller task to send canister messages from the directly to this client connection handler task
                // which will then forward it to the client via the WebSocket connection
                let (message_for_client_tx, mut message_for_client_rx): (
                    Sender<IcWsConnectionUpdate>,
                    Receiver<IcWsConnectionUpdate>,
                ) = mpsc::channel(100);

                let wait_for_cancellation = self.token.cancelled();
                tokio::pin!(wait_for_cancellation);
                let mut ic_websocket_setup = false;
                'handler_loop: loop {
                    select! {
                        // bias select! to check token cancellation first
                        // with 'biased', async functions are polled in the order in which they appear
                        biased;
                        // waits for the token to be cancelled
                        _ = &mut wait_for_cancellation => {
                            let graceful_shutdown_span = span!(parent: &Span::current(), Level::DEBUG, "graceful_shutdown");
                            // close the WebSocket connection
                            if let Err(e) = ws_write.close().await {
                                graceful_shutdown_span.in_scope(|| {
                                    error!("Error closing the WS connection: {:?}", e);
                                });
                            }
                            graceful_shutdown_span.in_scope(|| {
                                debug!("Terminating client connection handler task due to graceful shutdown");
                            });
                            self.send_connection_state_to_clients_manager(
                                IcWsConnectionState::Closed(
                                    (
                                        self.key.clone().expect("must be some by now"),
                                        self.canister_id
                                            .read()
                                            .await
                                            .expect("must be some by now")
                                            .clone(),
                                        graceful_shutdown_span
                                    )
                                )
                            )
                            .await;
                            break 'handler_loop;
                        },
                        // wait for canister message to send to client
                        Some(poller_message) = message_for_client_rx.recv() => {
                            match poller_message {
                                // check if the poller task detected an error from the CDK
                                IcWsConnectionUpdate::Message((canister_message, parent_span)) => {
                                    let message_for_client_span = span!(parent: &parent_span, Level::TRACE, "message_for_client");
                                    let message_key = MessageReference::new(
                                        self.canister_id
                                            .read()
                                            .await
                                            .expect("must be some by now")
                                            .clone(),
                                        get_nonce_from_message(&canister_message.key).expect("poller relayed a message not correcly formatted")
                                    );
                                    let mut outgoing_canister_message_events = OutgoingCanisterMessageEvents::new(Some(EventsReference::MessageReference(message_key)), EventsCollectionType::CanisterMessage, OutgoingCanisterMessageEventsMetrics::default());
                                    outgoing_canister_message_events.metrics.set_received_canister_message();
                                    // relay canister message to client, cbor encoded
                                    match to_vec(&canister_message) {
                                        Ok(bytes) => {
                                            match send_ws_message_to_client(&mut ws_write, Message::Binary(bytes)).await {
                                                Ok(_) => {
                                                    outgoing_canister_message_events.metrics.set_message_sent_to_client();
                                                    message_for_client_span.in_scope(|| {
                                                        trace!("Message sent to client");
                                                    });
                                                }
                                                Err(e) => {
                                                    message_for_client_span.in_scope(|| {
                                                        outgoing_canister_message_events.metrics.set_message_sent_to_client();
                                                        error!("Could not send message to client. Error: {:?}", e);
                                                    });
                                                }
                                            }
                                        },
                                        Err(e) => {
                                            outgoing_canister_message_events.metrics.set_no_message_sent_to_client();
                                            message_for_client_span.in_scope(|| {
                                                error!("Could not serialize canister message. Error: {:?}", e);
                                            });
                                        }
                                    }
                                    self.events_channel_tx.send(Box::new(outgoing_canister_message_events)).await.expect("analyzer's side of the channel dropped");
                                },
                                // the poller task terminates all the client connection tasks connected to that poller
                                IcWsConnectionUpdate::Error(e) => {
                                    let poller_error_span = span!(parent: &Span::current(), Level::DEBUG, "poller_error");
                                    // close the WebSocket connection
                                    if let Err(e) = ws_write.close().await {
                                        poller_error_span.in_scope(|| {
                                            error!("Error closing the WS connection: {:?}", e);
                                        });
                                    }
                                    poller_error_span.in_scope(|| {
                                        error!("Terminating client connection handler task. Error: {}", e);
                                    });
                                    break 'handler_loop;
                                }
                            }
                        },
                        // wait for incoming message from client
                        ws_message = ws_read.try_next() => {
                            match ws_message {
                                // handle message sent from client via WebSocket
                                Ok(Some(message)) => {
                                    // check if the WebSocket connection is closed
                                    if message.is_close() {
                                        let client_disconnection_span = span!(parent: &Span::current(), Level::DEBUG, "client_disconnection");
                                        client_disconnection_span.in_scope(|| {
                                            debug!("Terminating client connection handler task due to client disconnection");
                                        });

                                        // let the main task know that it should remove the client's session from the WS Gateway state
                                        self.send_connection_state_to_clients_manager(
                                            IcWsConnectionState::Closed(
                                                (
                                                    self.key.clone().expect("must be some by now"),
                                                    self.canister_id
                                                        .read()
                                                        .await
                                                        .expect("must be some by now")
                                                        .clone(),
                                                    client_disconnection_span
                                                ),
                                            )
                                        )
                                        .await;
                                        break 'handler_loop;
                                    }
                                    // check if the IC WebSocket connection hasn't been established yet
                                    if !ic_websocket_setup {
                                        match self.inspect_ic_ws_open_message(message.clone()).await {
                                            // if the IC WS connection is setup, create a new client session and send it to the main task
                                            Ok(IcWsConnectionState::Requested(client_key)) => {
                                                ic_websocket_setup_span.in_scope(|| {
                                                    debug!("Validated WS open message");
                                                });

                                                ic_websocket_setup = true;

                                                self.key = Some(client_key.clone());
                                                let client_session = ClientSession::new(
                                                    self.id,    // used as reference for events metrics of connection establishment in gateway server
                                                    client_key, // used to identify the client in poller
                                                    self.canister_id    // used to specify which canister the client wants to connect to
                                                        .read()
                                                        .await
                                                        .expect("must be some by now")
                                                        .clone(),
                                                    message_for_client_tx.clone(),  // used to send canister updates from the demux to the client connection handler
                                                    Span::current(),
                                                );

                                                let setup_connection_async = async {
                                                    self.send_connection_state_to_clients_manager(IcWsConnectionState::Setup(
                                                        client_session,
                                                    ))
                                                    .await;
                                                    // at this point we are NOT guaranteed that the gateway server received the client session

                                                    // if the poller is already running, it might receive the first canister message before it receives the channel from the gateway server.
                                                    // relaying the request to the IC after sending the client session to gateway server might give it enough time to send the client channel to the poller
                                                    // but this is not guaranteed
                                                    // TODO: evaluate whether it is necessary to wait until the poller receives the channel or if we can assume that
                                                    //       time_to_relay_request_to_ic + time_to_poll_first_message >> time_to_send_channel_to_poller
                                                    if let Err(e) = self.relay_call_request_to_ic(message).await {
                                                        error!("Could not relay request to IC. Error: {:?}", e);
                                                    }
                                                };
                                                setup_connection_async.instrument(ic_websocket_setup_span.clone()).await;

                                                request_connection_setup_events
                                                    .metrics
                                                    .set_ws_connection_setup();
                                                self.events_channel_tx
                                                    .send(Box::new(request_connection_setup_events.clone()))
                                                    .await
                                                    .expect("analyzer's side of the channel dropped");
                                            }
                                            // in case of other errors, we report them and terminate the connection handler task
                                            Err(e) => {
                                                warn!("IC WS setup failed. Error: {:?}", e);
                                                break 'handler_loop;
                                            }
                                            Ok(variant) => unreachable!("handle_ic_ws_setup should not return variant: {:?}", variant)
                                        }
                                    } else {
                                        let client_message_span = span!(parent: &Span::current(), Level::TRACE, "client_message");
                                        // relay the envelope to the IC and the response back to the client
                                        if let Err(e) = self.handle_ws_message(message).instrument(client_message_span).await {
                                            warn!("Handling of WebSocket message failed. Error: {:?}", e);
                                            break 'handler_loop;
                                        }
                                    }
                                },
                                // in this case, client's session should have been cleaned up on the WS Gateway state already
                                // once the connection handler received Message::Close
                                // just to be sure, send the cleanup message again
                                // TODO: figure out if this is necessary or can be ignored
                                Ok(None) => {
                                    let websocket_error_span = span!(parent: &Span::current(), Level::DEBUG, "websocket_error");
                                    websocket_error_span.in_scope(|| {
                                        warn!("Client WebSocket connection already closed");
                                    });
                                    self.send_connection_state_to_clients_manager(
                                        IcWsConnectionState::Closed(
                                            (
                                                self.key.clone().expect("must be some by now"),
                                                self.canister_id
                                                    .read()
                                                    .await
                                                    .expect("must be some by now")
                                                    .clone(),
                                                websocket_error_span
                                            )
                                        )
                                    )
                                    .await;
                                    break 'handler_loop;
                                },
                                // the client's still needs to be cleaned up so it is necessary to return the client id
                                Err(e) => {
                                    let websocket_error_span = span!(parent: &Span::current(), Level::DEBUG, "websocket_error");
                                    websocket_error_span.in_scope(|| {
                                        warn!("Client WebSocket connection error: {:?}", e);
                                    });
                                    // let the main task know that it should remove the client's session from the WS Gateway state
                                    self.send_connection_state_to_clients_manager(
                                        IcWsConnectionState::Closed(
                                            (
                                                self.key.clone().expect("must be some by now"),
                                                self.canister_id
                                                    .read()
                                                    .await
                                                    .expect("must be some by now")
                                                    .clone(),
                                                websocket_error_span
                                            )
                                        )
                                    )
                                    .await;
                                    break 'handler_loop;
                                }
                            };
                        }
                    }
                }
            },
            // no cleanup needed on the WS Gateway has the client's session has never been created
            Err(e) => {
                info!("Refused WebSocket connection {:?}", e);
            },
        }
        debug!("Terminated client connection handler task");
    }

    async fn inspect_ic_ws_open_message(
        &self,
        ws_message: Message,
    ) -> Result<IcWsConnectionState, IcWsError> {
        let client_request = get_client_request(ws_message)?;
        // if the canister_id field is None, it means that the handler hasn't received any envelopes yet from the client
        if self.canister_id.read().await.is_none() {
            // the first envelope should have content of variant Call, which contains canister_id
            if let EnvelopeContent::Call { canister_id, .. } = *client_request.envelope.content {
                // replace the field with the canister_id received in the first envelope
                // this should not be updated anymore
                self.canister_id.write().await.replace(canister_id);
            } else {
                return Err(IcWsError::Initialization(String::from(
                    "first message from client should contain canister id in envelope's content and should be of Call variant",
                )));
            }
        }
        // from here on, self.canister_id must not be None
        let client_principal = client_request.envelope.content.sender().to_owned();

        if let EnvelopeContent::Call { arg, .. } = &*client_request.envelope.content {
            let (ws_open_arguments,): (CanisterWsOpenArguments,) =
                decode_args(arg).map_err(|e| {
                    IcWsError::Initialization(format!(
                        "arg field of envelope's content has the wrong type: {:?}",
                        e.to_string()
                    ))
                })?;
            let client_key = ClientKey::new(client_principal, ws_open_arguments.client_nonce);

            // consider the IC WS connection as requested
            Ok(IcWsConnectionState::Requested(client_key))
        } else {
            Err(IcWsError::Initialization(String::from(
                "first message from client should contain arg in envelope's content",
            )))
        }
    }

    /// relays relays the client's request to the IC and then sends the response back to the client via WS
    /// the caller does not need to check the state fo the response, it just relays the messages back and forth
    async fn handle_ws_message(&self, message: Message) -> Result<(), IcWsError> {
        self.relay_call_request_to_ic(message).await?;
        Ok(())
    }

    /// relays the client's request to the IC only if the content of the envelope is of the Call variant
    async fn relay_call_request_to_ic(&self, message: Message) -> Result<(), IcWsError> {
        let client_request = get_client_request(message)?;
        if let EnvelopeContent::Call { .. } = *client_request.envelope.content {
            let serialized_envelope = serialize(client_request.envelope)?;

            let canister_id = self.canister_id.read().await.expect("must be some by now");

            // relay the envelope to the IC
            self.relay_envelope_to_canister(serialized_envelope, canister_id.clone())
                .await
                .map_err(|e| IcWsError::Initialization(e.to_string()))?;

            // there is no need to relay the response back to the client as the response to a request to the /call enpoint is not certified by the canister
            // and therefore could be manufactured by the gateway

            trace!("Relayed serialized envelope to canister");
            Ok(())
        } else {
            Err(IcWsError::Initialization(String::from(
                "Gateway can only relay envelopes with content of Call variant",
            )))
        }
    }

    async fn relay_envelope_to_canister(
        &self,
        serialized_envelope: Vec<u8>,
        canister_id: Principal,
    ) -> Result<(), AgentError> {
        self.agent
            .update_signed(canister_id, serialized_envelope)
            .await?;
        return Ok(());
    }

    async fn send_connection_state_to_clients_manager(
        &self,
        connection_state: IcWsConnectionState,
    ) {
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

fn get_client_request<'a>(message: Message) -> Result<ClientRequest<'a>, IcWsError> {
    if let Message::Binary(bytes) = message {
        if let Ok(client_request) = from_slice(&bytes) {
            return Ok(client_request);
        } else {
            return Err(IcWsError::WebSocket(String::from(
                "ws message from client is not of type ClientRequest",
            )));
        }
    }
    Err(IcWsError::WebSocket(String::from(
        "ws message from client is not binary encoded",
    )))
}

fn serialize<S: Serialize>(message: S) -> Result<Vec<u8>, IcWsError> {
    let mut serialized_message = Vec::new();
    let mut serializer = serde_cbor::Serializer::new(&mut serialized_message);
    serializer.self_describe().map_err(|e| {
        IcWsError::WebSocket(format!(
            "could not write self-describe tag to stream. Error: {:?}",
            e.to_string()
        ))
    })?;
    message.serialize(&mut serializer).map_err(|e| {
        IcWsError::WebSocket(format!(
            "could not serialize message. Error: {:?}",
            e.to_string()
        ))
    })?;
    Ok(serialized_message)
}

async fn send_ws_message_to_client<S: AsyncRead + AsyncWrite + Unpin>(
    ws_write: &mut SplitSink<WebSocketStream<S>, Message>,
    message: Message,
) -> Result<(), IcWsError> {
    if let Err(e) = ws_write.send(message).await {
        return Err(IcWsError::WebSocket(e.to_string()));
    }
    Ok(())
}

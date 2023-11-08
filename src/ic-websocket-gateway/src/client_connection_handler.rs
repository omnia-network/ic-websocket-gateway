use crate::{
    canister_methods::{CanisterWsOpenArguments, ClientId, ClientKey},
    canister_poller::IcWsConnectionUpdate,
    events_analyzer::{Events, EventsCollectionType, EventsReference, MessageReference},
    gateway_server::ClientSession,
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
use tracing::{debug, error, info, trace, warn};

/// Message sent by the client using the custom @dfinity/agent (via WS)
#[derive(Serialize, Deserialize)]
struct ClientRequest<'a> {
    /// Envelope of the signed request to the IC
    envelope: Envelope<'a>,
}

/// Message sent back to the client via WS
#[derive(Serialize, Deserialize)]
struct ClientResponse {
    /// HTTP response produced by the IC
    payload: HttpResponsePayload,
    /// Used by the client to identify which request this response corresponds to
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
}

/// HTTP response produced by the IC
#[derive(Serialize, Deserialize)]
pub struct HttpResponsePayload {
    /// The HTTP status code.
    pub status: u16,
    /// The MIME type of `content`.
    pub content_type: Option<String>,
    /// The body of the error.
    #[serde(with = "serde_bytes")]
    pub content: Vec<u8>,
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
    Closed((ClientKey, Principal)),
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
                            self.send_connection_state_to_clients_manager(
                                IcWsConnectionState::Closed(
                                    (
                                        self.key.clone().expect("must be some by now"),
                                        self.canister_id
                                            .read()
                                            .await
                                            .expect("must be some by now")
                                            .clone()
                                    )
                                )
                            )
                            .await;
                            // close the WebSocket connection
                            if let Err(e) = ws_write.close().await {
                                error!("Error closing the WS connection: {:?}", e);
                            }
                            debug!("Terminating client connection handler task due to graceful shutdown");
                            break 'handler_loop;
                        },
                        // wait for canister message to send to client
                        Some(poller_message) = message_for_client_rx.recv() => {
                            match poller_message {
                                // check if the poller task detected an error from the CDK
                                IcWsConnectionUpdate::Message(canister_message) => {
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
                                            send_ws_message_to_client(&mut ws_write, Message::Binary(bytes)).await;
                                            outgoing_canister_message_events.metrics.set_message_sent_to_client();
                                            trace!("Message with key: {:?} sent to client", canister_message.key);
                                        },
                                        Err(e) => {
                                            outgoing_canister_message_events.metrics.set_no_message_sent_to_client();
                                            error!("Could not serialize canister message. Error: {:?}", e);
                                        }
                                    }
                                    self.events_channel_tx.send(Box::new(outgoing_canister_message_events)).await.expect("analyzer's side of the channel dropped");
                                },
                                // the poller task terminates all the client connection tasks connected to that poller
                                IcWsConnectionUpdate::Error(e) => {
                                    // close the WebSocket connection
                                    ws_write.close().await.unwrap();
                                    error!("Terminating client connection handler task. Error: {}", e);
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
                                        // let the main task know that it should remove the client's session from the WS Gateway state
                                        self.send_connection_state_to_clients_manager(
                                            IcWsConnectionState::Closed(
                                            (
                                                self.key.clone().expect("must be some by now"),
                                                self.canister_id
                                                    .read()
                                                    .await
                                                    .expect("must be some by now")
                                                    .clone()
                                                ),
                                            )
                                        )
                                        .await;
                                        debug!("Terminating client connection handler task due to client disconnection");
                                        break 'handler_loop;
                                    }
                                    // check if the IC WebSocket connection hasn't been established yet
                                    if !ic_websocket_setup {
                                        match self.inspect_ic_ws_setup_message(message.clone()).await {
                                            // if the IC WS connection is setup, create a new client session and send it to the main task
                                            Ok(IcWsConnectionState::Requested(client_key)) => {
                                                ic_websocket_setup = true;
                                                self.key = Some(client_key.clone());
                                                let client_session = ClientSession::new(
                                                    self.id,
                                                    client_key, // TODO: determine if this is still needed or we can use client_id instead
                                                    self.canister_id
                                                        .read()
                                                        .await
                                                        .expect("must be some by now")
                                                        .clone(),
                                                    message_for_client_tx.clone(),
                                                );
                                                self.send_connection_state_to_clients_manager(IcWsConnectionState::Setup(
                                                    client_session,
                                                ))
                                                .await;

                                                // relay the request to the IC
                                                // done it after sending client session to gateway server so that it has time to start a new poller
                                                // or send the client channel to an existing one
                                                if let Err(e) = self.relay_call_request_to_ic(message).await {
                                                    warn!("Could not relay request to IC. Error: {:?}", e);
                                                }

                                                debug!("Created new client session");
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
                                            Ok(variant) => error!("handle_ic_ws_setup should not return variant: {:?}", variant)
                                        }
                                    } else {
                                        // relay the envelope to the IC and the response back to the client
                                        if let Err(e) = self.handle_ws_message(message).await {
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
                                    self.send_connection_state_to_clients_manager(
                                        IcWsConnectionState::Closed(
                                            (
                                                self.key.clone().expect("must be some by now"),
                                                self.canister_id
                                                    .read()
                                                    .await
                                                    .expect("must be some by now")
                                                    .clone()
                                            )
                                        )
                                    )
                                    .await;
                                    warn!("Client WebSocket connection already closed");
                                    break 'handler_loop;
                                },
                                // the client's still needs to be cleaned up so it is necessary to return the client id
                                Err(e) => {
                                    // let the main task know that it should remove the client's session from the WS Gateway state
                                    self.send_connection_state_to_clients_manager(
                                        IcWsConnectionState::Closed(
                                            (
                                                self.key.clone().expect("must be some by now"),
                                                self.canister_id
                                                    .read()
                                                    .await
                                                    .expect("must be some by now")
                                                    .clone()
                                            )
                                        )
                                    )
                                    .await;
                                    warn!("Client WebSocket connection error: {:?}", e);
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
    }

    async fn inspect_ic_ws_setup_message(
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

            trace!(
                "Relayed serialized envelope of type Call to canister with principal: {:?}",
                canister_id.to_string()
            );
            Ok(())
        } else {
            Err(IcWsError::Initialization(String::from(
                "gateway can only relay envelopes with content of Call variant",
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
            "could not write sel-describe tag to stream. Error: {:?}",
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
) {
    if let Err(e) = ws_write.send(message).await {
        // TODO: graceful shutdown fo client task
        error!("Could not send message to client: {:?}", e);
    }
}

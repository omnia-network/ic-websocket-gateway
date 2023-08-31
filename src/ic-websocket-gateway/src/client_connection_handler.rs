use crate::{
    canister_methods::{CanisterWsOpenResult, CanisterWsOpenResultValue},
    canister_poller::{get_nonce_from_message, IcWsConnectionUpdate},
    events_analyzer::{Events, EventsCollectionType, EventsReference},
    gateway_server::GatewaySession,
    metrics::client_connection_handler_metrics::{
        OutgoingCanisterMessageEvents, OutgoingCanisterMessageEventsMetrics,
        RequestConnectionSetupEvents, RequestConnectionSetupEventsMetrics,
    },
};
use candid::{Decode, Principal};
use futures_util::{stream::SplitSink, SinkExt, StreamExt, TryStreamExt};
use ic_agent::{
    agent::{Envelope, Replied, RequestStatusResponse, CONTENT_TYPE},
    Agent,
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
use tracing::{debug, error, info, warn};

/// Message sent by the client using the custom @dfinity/agent (via WS)
#[derive(Serialize, Deserialize)]
struct ClientRequest<'a> {
    /// Envelope of the signed request to the IC
    envelope: Envelope<'a>,
    /// Used by the client to identify which response corresponds to this request
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
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
/// - established
/// - closed
#[derive(Debug, Clone)]
pub enum IcWsConnectionState {
    /// IC WebSocket connection between client and canister established
    Established(GatewaySession),
    Setup(CanisterWsOpenResultValue),
    Init,
    /// WebSocket connection between client and WS Gateway closed
    Closed(u64),
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
    id: u64,
    agent: Arc<Agent>,
    client_connection_handler_tx: Sender<IcWsConnectionState>,
    events_channel_tx: Sender<Box<dyn Events + Send>>,
    token: CancellationToken,
    // the client tells which canister it wants to connect to in the first envelope it sends via WS
    canister_id: Arc<RwLock<Option<Principal>>>,
}

impl ClientConnectionHandler {
    pub fn new(
        id: u64,
        agent: Arc<Agent>,
        client_connection_handler_tx: Sender<IcWsConnectionState>,
        events_channel_tx: Sender<Box<dyn Events + Send>>,
        token: CancellationToken,
    ) -> Self {
        Self {
            id,
            agent,
            client_connection_handler_tx,
            events_channel_tx,
            token,
            canister_id: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn handle_stream<S: AsyncRead + AsyncWrite + Unpin>(&self, stream: S) {
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
                loop {
                    select! {
                        // bias select! to check token cancellation first
                        // with 'biased', async functions are polled in the order in which they appear
                        biased;
                        // waits for the token to be cancelled
                        _ = &mut wait_for_cancellation => {
                            self.send_connection_state_to_clients_manager(
                                IcWsConnectionState::Closed(self.id)
                            )
                            .await;
                            // close the WebSocket connection
                            if let Err(e) = ws_write.close().await {
                                error!("Error closing the WS connection: {:?}", e);
                            }
                            debug!("Terminating client connection handler task");
                            break;
                        },
                        // wait for canister message to send to client
                        Some(poller_message) = message_for_client_rx.recv() => {
                            match poller_message {
                                // check if the poller task detected an error from the CDK
                                IcWsConnectionUpdate::Message(canister_message) => {
                                    let message_nonce = get_nonce_from_message(&canister_message.key).expect("poller relayed a message not correcly formatted");
                                    let mut outgoing_canister_message_events = OutgoingCanisterMessageEvents::new(Some(EventsReference::MessageNonce(message_nonce)), EventsCollectionType::CanisterMessage, OutgoingCanisterMessageEventsMetrics::default());
                                    outgoing_canister_message_events.metrics.set_received_canister_message();
                                    debug!("Sending message with key: {:?} to client", canister_message.key);
                                    // relay canister message to client, cbor encoded
                                    match to_vec(&canister_message) {
                                        Ok(bytes) => {
                                            send_ws_message_to_client(&mut ws_write, Message::Binary(bytes)).await;
                                            outgoing_canister_message_events.metrics.set_message_sent_to_client();
                                            debug!("Message with key: {:?} sent to client", canister_message.key);
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
                                    break;
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
                                        self.send_connection_state_to_clients_manager(IcWsConnectionState::Closed(
                                            self.id,
                                        ))
                                        .await;
                                        break;
                                    }
                                    // check if the IC WebSocket connection hasn't been established yet
                                    if !ic_websocket_setup {
                                        match self.handle_ic_ws_setup(message, &mut ws_write).await {
                                            // if the IC WS connection is successfully established, register state in client manager
                                            Ok(IcWsConnectionState::Setup(canister_ws_open_result_value)) => {
                                                ic_websocket_setup = true;
                                                let gateway_session = GatewaySession::new(
                                                    self.id,
                                                    canister_ws_open_result_value.client_principal, // TODO: determine if this is still needed or we can use client_id instead
                                                    self.canister_id
                                                        .read()
                                                        .await
                                                        .expect("must be some by now")
                                                        .clone(),
                                                    message_for_client_tx.clone(),
                                                    canister_ws_open_result_value.nonce,
                                                );
                                                self.send_connection_state_to_clients_manager(IcWsConnectionState::Established(
                                                    gateway_session,
                                                ))
                                                .await;
                                                debug!("Created new client session");
                                                request_connection_setup_events
                                                    .metrics
                                                    .set_ws_connection_setup();
                                                self.events_channel_tx
                                                    .send(Box::new(request_connection_setup_events.clone()))
                                                    .await
                                                    .expect("analyzer's side of the channel dropped");
                                            }
                                            // if the connection hasn't been established yet, continue
                                            Ok(IcWsConnectionState::Init) => continue,
                                            // in case of other errors, we report them and terminate the connection handler task
                                            Err(e) => {
                                                warn!("{:?}", e);
                                                break;
                                            }
                                            Ok(variant) => error!("handle_ic_ws_setup should not return variant: {:?}", variant)
                                        }
                                    } else {
                                        // relay the envelope to the IC and the response back to the client
                                        if let Err(e) = self.handle_ws_message(message, &mut ws_write).await {
                                            warn!("{:?}", e);
                                            break;
                                        }
                                    }
                                },
                                // in this case, client's session should have been cleaned up on the WS Gateway state already
                                // once the connection handler received Message::Close
                                // just to be sure, send the cleanup message again
                                // TODO: figure out if this is necessary or can be ignored
                                Ok(None) => {
                                    self.send_connection_state_to_clients_manager(IcWsConnectionState::Closed(self.id))
                                        .await;
                                    warn!("Client WebSocket connection already closed");
                                },
                                // the client's still needs to be cleaned up so it is necessary to return the client id
                                Err(e) => {
                                    // let the main task know that it should remove the client's session from the WS Gateway state
                                    self.send_connection_state_to_clients_manager(IcWsConnectionState::Closed(self.id))
                                        .await;
                                    warn!("Client WebSocket connection error: {:?}", e);
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

    async fn handle_ic_ws_setup<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        ws_message: Message,
        ws_write: &mut SplitSink<WebSocketStream<S>, Message>,
    ) -> Result<IcWsConnectionState, IcWsError> {
        let client_request = get_client_request(ws_message)?;
        // if the canister_id field is None, it means that the handler hasn't received any envelopes yet from the client
        if self.canister_id.read().await.is_none() {
            // the first envelope should have content of variant Call, which contains canister_id
            let canister_id = client_request.envelope.content.canister_id().ok_or(
                // if the content of the first envelope does not contain the canister_id field, the client did not follow the IC WS establishment
                IcWsError::Initialization(String::from(
                    "first message from client should contain canister id in envelope's content",
                )),
            )?;
            // replace the field with the canister_id received in the first envelope
            // this should not be updated anymore
            self.canister_id.write().await.replace(canister_id);
        }
        // from here on, self.canister_id must not be None

        // relay the request to the IC and the response back to the client via WS and determine whether the IC WS connection has been setup
        if let Some(request_status_response) = self
            .relay_request_response(client_request, ws_write)
            .await?
        {
            // determine the status of the IC WS connection establishment
            self.get_ic_ws_establishment_status(request_status_response)
                .await
        } else {
            // if request_status_response is None, the payload's content was of the Call variant and therefore
            // the client has just started the IC WS establishment
            Ok(IcWsConnectionState::Init)
        }
    }

    /// relays relays the client's request to the IC and then sends the response back to the client via WS
    /// the caller does not need to check the state fo the response, it just relays the messages back and forth
    async fn handle_ws_message<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        message: Message,
        ws_write: &mut SplitSink<WebSocketStream<S>, Message>,
    ) -> Result<(), IcWsError> {
        let client_request = get_client_request(message)?;
        self.relay_request_response(client_request, ws_write)
            .await?;
        Ok(())
    }

    /// relays the client's request to the IC and then sends the response back to the client via WS
    /// if the content of the envelope in the request is of variant ReadState, returns RequestStatusResponse so that the caller can determine whether the request has been processed
    async fn relay_request_response<'a, S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        client_request: ClientRequest<'a>,
        ws_write: &mut SplitSink<WebSocketStream<S>, Message>,
    ) -> Result<Option<RequestStatusResponse>, IcWsError> {
        let serialized_envelope = serialize(client_request.envelope)?;

        // relay the envelope to the IC
        let (http_response, request_status_response) = self
            .agent
            .relay_envelope_to_canister(
                serialized_envelope,
                self.canister_id
                    .read()
                    .await
                    .expect("must be some by now")
                    .clone(),
            )
            .await
            .map_err(|e| IcWsError::Initialization(e.to_string()))?;

        // send the HTTP response back to the client
        let payload = HttpResponsePayload {
            status: http_response.status.into(),
            content_type: http_response
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|x| x.to_string()),
            content: http_response.body.clone(),
        };
        let client_response = ClientResponse {
            payload,
            nonce: client_request.nonce,
        };
        let serialized_response = serialize(client_response)?;
        send_ws_message_to_client(ws_write, Message::Binary(serialized_response)).await;

        Ok(request_status_response)
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

    /// determines the status of the response from the IC following a request to /read_state
    async fn get_ic_ws_establishment_status(
        &self,
        request_status_response: RequestStatusResponse,
    ) -> Result<IcWsConnectionState, IcWsError> {
        match request_status_response {
            RequestStatusResponse::Replied {
                reply: Replied::CallReplied(result),
            } => {
                // parse the body in order to get the nonce which will be needed in case it has to start a new poller
                let canister_ws_open_result_value = Decode!(&result, CanisterWsOpenResult)
                    .map_err(|e| {
                        IcWsError::Initialization(format!(
                            "client must call ws_open before other methods. Error: {:?}",
                            e
                        ))
                    })?
                    .map_err(|e| IcWsError::Initialization(format!("ws_open failed: {:?}", e)))?;
                if !self.token.is_cancelled() {
                    // return the ws_open result value so that the handler can construct a new client session and send it to the client's state manager
                    Ok(IcWsConnectionState::Setup(canister_ws_open_result_value))
                } else {
                    // if the gateway has already started the graceful shutdown, we have to prevent new clients from connecting
                    Err(IcWsError::WebSocket(String::from(
                        "Preventing client connection handler task to establish new WS connection",
                    )))
                }
            },
            RequestStatusResponse::Rejected(e) => Err(IcWsError::Initialization(e.to_string())),
            RequestStatusResponse::Done => Err(IcWsError::Initialization(String::from(
                "IC WS connection already established",
            ))),
            // if request_status_response is of the variant Unknown, Received, or Processing,
            // the IC WS connection is still in the setup phase
            // TODO: Unknown should be treated separately as it could mean both that the IC hasn't processed yet a request with the given RequestId (but it will) or that such a request does not exist
            _ => Ok(IcWsConnectionState::Init),
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

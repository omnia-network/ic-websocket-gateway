use crate::{
    canister_methods::CanisterWsOpenResult,
    canister_poller::{get_nonce_from_message, IcWsConnectionUpdate},
    events_analyzer::{Events, EventsCollectionType, EventsReference},
    gateway_server::GatewaySession,
    metrics::client_connection_handler_metrics::{
        ConfirmedConnectionSetupEvents, ConfirmedConnectionSetupEventsMetrics,
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
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// message sent by the client using the custom @dfinity/agent
#[derive(Serialize, Deserialize)]
struct ClientMessage<'a> {
    envelope: Envelope<'a>,
    nonce: u64,
}

/// A HTTP response from a replica
#[derive(Serialize, Deserialize)]
pub struct HttpResponsePayload {
    /// The HTTP status code.
    pub status: u16,
    /// The MIME type of `content`.
    pub content_type: Option<String>,
    /// The body of the error.
    pub content: Vec<u8>,
}

/// possible states of the WebSocket connection:
/// - established
/// - closed
/// - error
#[derive(Debug, Clone)]
pub enum WsConnectionState {
    /// WebSocket connection between client and WS Gateway established
    // does not imply that the IC WebSocket connection has also been established
    Established(GatewaySession),
    Setup,
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

pub struct ClientConnectionHandler {
    id: u64,
    agent: Arc<Agent>,
    client_connection_handler_tx: Sender<WsConnectionState>,
    events_channel_tx: Sender<Box<dyn Events + Send>>,
    token: CancellationToken,
    // the client tells which canister it wants to connect to in the first envelope it sends via WS
    canister_id: Arc<RwLock<Option<Principal>>>,
}

impl ClientConnectionHandler {
    pub fn new(
        id: u64,
        agent: Arc<Agent>,
        client_connection_handler_tx: Sender<WsConnectionState>,
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
                let mut request_connection_setup_events = Some(RequestConnectionSetupEvents::new(
                    Some(EventsReference::ClientId(self.id)),
                    EventsCollectionType::NewClientConnection,
                    RequestConnectionSetupEventsMetrics::default(),
                ));

                request_connection_setup_events
                    .as_mut()
                    .expect("must have connection setup events for first message")
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
                let mut ic_websocket_established = false;
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
                                IcWsConnectionUpdate::Established => {
                                    let mut confirmed_connection_setup_events = ConfirmedConnectionSetupEvents::new(
                                        Some(EventsReference::ClientId(self.id)),
                                        EventsCollectionType::NewClientConnection,
                                        ConfirmedConnectionSetupEventsMetrics::default(),
                                    );
                                    confirmed_connection_setup_events.metrics.set_received_confirmation_from_poller();
                                    debug!("Client established IC WebSocket connection");
                                    // let the client know that the IC WS connection is setup correctly
                                    send_ws_message_to_client(
                                        &mut ws_write,
                                        Message::Text("1".to_string()),
                                    )
                                    .await;
                                    confirmed_connection_setup_events.metrics.set_confirmation_sent_to_client();
                                    self.events_channel_tx
                                        .send(Box::new(confirmed_connection_setup_events))
                                        .await
                                        .expect("analyzer's side of the channel dropped");
                                }
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
                            match self.handle_incoming_ws_message(
                                ws_message,
                                ic_websocket_established,
                                request_connection_setup_events,
                                &mut ws_write,
                                message_for_client_tx.clone(),
                            ).await {
                                // if the connection is successfully established, the following messages are not the first
                                WsConnectionState::Established(_) => ic_websocket_established = true,
                                WsConnectionState::Setup => ic_websocket_established = false,
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
                            // after the first time 'handle_incoming_ws_message' is called, we do not need to collect connection setup events anymore
                            // so we always pass None
                            request_connection_setup_events = None;
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
        ic_websocket_established: bool,
        mut request_connection_setup_events: Option<RequestConnectionSetupEvents>,
        mut ws_write: &mut SplitSink<WebSocketStream<S>, Message>,
        message_for_client_tx: Sender<IcWsConnectionUpdate>,
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
                // check if the IC WebSocket connection hasn't been established yet
                if !ic_websocket_established {
                    if let Message::Binary(bytes) = message {
                        if let Ok(ClientMessage { envelope, nonce }) = from_slice(&bytes) {
                            // if the canister_id field is None, it means that the handler hasn't received any envelopes yet from the client
                            if self.canister_id.read().await.is_none() {
                                // the first envelope should have content of variant Call, which contains canister_id
                                if let Some(canister_id) = envelope.content.canister_id() {
                                    // replace the field with the canister_id received in the first envelope
                                    // this should not be updated anymore
                                    self.canister_id.write().await.replace(canister_id);
                                } else {
                                    // if the content of the first envelope does not contain the canister_id field, the client did not follow the IC WS establishment
                                    return WsConnectionState::Error(IcWsError::Initialization(String::from(
                                        "first message from client should contain canister id in envelope's content",
                                    )));
                                }
                            }

                            // from here on, self.canister_id must not be None
                            let mut serialized_envelope = Vec::new();
                            let mut serializer =
                                serde_cbor::Serializer::new(&mut serialized_envelope);
                            serializer.self_describe().unwrap();
                            envelope.serialize(&mut serializer).unwrap();

                            // relay the envelope to the IC
                            match self
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
                            {
                                Ok((http_response, request_status_response)) => {
                                    // send http response to client
                                    let http_response = HttpResponsePayload {
                                        status: http_response.status.into(),
                                        content_type: http_response
                                            .headers
                                            .get(CONTENT_TYPE)
                                            .and_then(|value| value.to_str().ok())
                                            .map(|x| x.to_string()),
                                        content: http_response.body,
                                    };

                                    let mut serialized_response = Vec::new();
                                    let mut serializer =
                                        serde_cbor::Serializer::new(&mut serialized_response);
                                    serializer.self_describe().unwrap();
                                    http_response.serialize(&mut serializer).unwrap();

                                    send_ws_message_to_client(
                                        &mut ws_write,
                                        Message::Binary(serialized_response),
                                    )
                                    .await;

                                    if let Some(request_status_response) = request_status_response {
                                        match request_status_response {
                                            RequestStatusResponse::Replied {
                                                reply: Replied::CallReplied(result),
                                            } => {
                                                // parse the body in order to get the nonce which will be needed in case it has to start a new poller
                                                let parsed_body =
                                                    Decode!(&result, CanisterWsOpenResult);
                                                let nonce = 0; // TODO: change CanisterWsOpenResult so that it returns 'nonce'
                                                println!("{:?}", parsed_body);

                                                if !self.token.is_cancelled() {
                                                    let gateway_session = GatewaySession::new(
                                                        self.id,
                                                        vec![], // TODO: determine if this is still needed or we can use client_id instead
                                                        self.canister_id
                                                            .read()
                                                            .await
                                                            .expect("must be some by now")
                                                            .clone(),
                                                        message_for_client_tx,
                                                        nonce,
                                                    );
                                                    WsConnectionState::Established(gateway_session)
                                                } else {
                                                    // if the gateway has already started the graceful shutdown, we have to prevent new clients from connecting
                                                    WsConnectionState::Error(IcWsError::WebSocket(String::from("Preventing client connection handler task to establish new WS connection")))
                                                }
                                            },
                                            RequestStatusResponse::Rejected(e) => {
                                                WsConnectionState::Error(IcWsError::Initialization(
                                                    e.to_string(),
                                                ))
                                            },
                                            RequestStatusResponse::Done => {
                                                WsConnectionState::Error(IcWsError::Initialization(
                                                    String::from(
                                                        "IC WS connection already established",
                                                    ),
                                                ))
                                            },
                                            // if request_status_response is of the variant Unknown, Received, or Processing,
                                            // the IC WS connection is still in the setup phase
                                            _ => WsConnectionState::Setup,
                                        }
                                    } else {
                                        // if request_status_response is None, the payload's content was of the Call variant and therefore
                                        // the IC WS connection is still in the setup phase
                                        WsConnectionState::Setup
                                    }
                                },
                                Err(e) => WsConnectionState::Error(IcWsError::Initialization(
                                    e.to_string(),
                                )),
                            }
                        } else {
                            WsConnectionState::Error(IcWsError::Initialization(String::from(
                                "message from client is not of type ClientMessage",
                            )))
                        }
                    } else {
                        WsConnectionState::Error(IcWsError::Initialization(String::from(
                            "message from client is not binary encoded",
                        )))
                    }
                //     // check if client followed the IC WebSocket connection establishment protocol
                //     match check_canister_init(&self.agent, message.clone()).await {
                //         Ok(CanisterWsOpenResultValue {
                //             client_key,
                //             canister_id,
                //             // nonce is used by a new poller to know which message nonce to start polling from (if needed)
                //             // the nonce is obtained from the canister every time a client connects and the ws_open is called by the WS Gateway
                //             nonce,
                //         }) => {
                //             request_connection_setup_events
                //                 .as_mut()
                //                 .expect("must have connection setup events for first message")
                //                 .metrics
                //                 .set_validated_first_message();
                //             // prevent adding a new client to the gateway state while shutting down
                //             // neeeded because wait_for_cancellation might be ready while handle_incoming_ws_message
                //             // is already executing
                //             if !self.token.is_cancelled() {
                //                 let gateway_session = GatewaySession::new(
                //                     self.id,
                //                     client_key,
                //                     canister_id,
                //                     message_for_client_tx,
                //                     nonce,
                //                 );
                //                 // instantiate a new GatewaySession and send it to the main thread
                //                 self.send_connection_state_to_clients_manager(
                //                     WsConnectionState::Established(gateway_session.clone()),
                //                 )
                //                 .await;
                //                 debug!("Created new client session");

                //                 // it is important to measure the instant of the establishment of the WS connection after sending the gateway session via the channel
                //                 // so that we can take into account eventual delays due to the channel being over capacity
                //                 // however, we still have to measure the latency of adding the client to the gateway state and eventually starting a new poller
                //                 // but this will be done in the gateway server component
                //                 request_connection_setup_events
                //                     .as_mut()
                //                     .expect("must have connection setup events for first message")
                //                     .metrics
                //                     .set_ws_connection_setup();
                //                 self.events_channel_tx
                //                     .send(Box::new(request_connection_setup_events.expect(
                //                         "must have connection setup events for first message",
                //                     )))
                //                     .await
                //                     .expect("analyzer's side of the channel dropped");
                //                 WsConnectionState::Established(gateway_session)
                //             } else {
                //                 // if the gateway has already started the graceful shutdown, we have to prevent new clients from connecting
                //                 WsConnectionState::Error(IcWsError::WebSocket(String::from("Preventing client connection handler task to establish new WS connection")))
                //             }
                //         },
                //         Err(e) => {
                //             // tell the client that the setup of the IC WS connection failed
                //             send_ws_message_to_client(
                //                 &mut ws_write,
                //                 Message::Text("0".to_string()),
                //             )
                //             .await;
                //             // if this branch is executed, the Ok branch is never been executed, hence the WS Gateway state
                //             // does not contain any session for this client and therefore there is no cleanup needed
                //             self.send_connection_state_to_clients_manager(
                //                 WsConnectionState::Error(IcWsError::Initialization(format!(
                //                     "Client did not follow IC WebSocket establishment protocol: {:?}",
                //                     e
                //                 ))),
                //             )
                //             .await;
                //             WsConnectionState::Error(IcWsError::Initialization(format!(
                //                 "Client did not follow IC WebSocket establishment protocol: {:?}",
                //                 e
                //             )))
                //         },
                //     }
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

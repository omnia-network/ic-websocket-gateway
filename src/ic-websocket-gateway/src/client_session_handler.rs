use crate::{
    canister_methods::{CanisterWsOpenArguments, ClientKey},
    canister_poller::{CanisterPoller, IcWsConnectionUpdate},
    events_analyzer::{self, Events, EventsCollectionType, EventsReference, MessageReference},
    manager::{GatewayState, PollerState},
    messages_demux::get_nonce_from_message,
    metrics::client_session_handler_metrics::{
        OutgoingCanisterMessageEvents, OutgoingCanisterMessageEventsMetrics,
        RequestConnectionSetupEvents, RequestConnectionSetupEventsMetrics,
    },
};
use candid::{decode_args, CandidType, Principal};
use dashmap::{mapref::entry::Entry, DashMap};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt, TryStreamExt,
};
use ic_agent::{
    agent::{Envelope, EnvelopeContent},
    Agent, AgentError,
};
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, to_vec};
use std::{fmt, sync::Arc};
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
    tungstenite::{client, Message},
    WebSocketStream,
};
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

/// possible states of an IC WebSocket session
#[derive(Debug, Clone)]
pub enum IcWsSessionState {
    Init,
    Setup(ClientKey, Principal, Sender<IcWsConnectionUpdate>),
    Closed,
}

impl Eq for IcWsSessionState {}

impl PartialEq for IcWsSessionState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (IcWsSessionState::Init, IcWsSessionState::Init) => true,
            (
                IcWsSessionState::Setup(client_key1, canister_id1, _),
                IcWsSessionState::Setup(client_key2, canister_id2, _),
            ) => client_key1 == client_key2 && canister_id1 == canister_id2,
            (IcWsSessionState::Closed, IcWsSessionState::Closed) => true,
            _ => false,
        }
    }
}

/// possible errors that can occur during an IC WebSocket session
#[derive(Debug, Clone)]
pub enum IcWsError {
    /// error due to the client not following the IC WS protocol
    IcWsProtocol(String),
    /// WebSocket error
    WebSocket(String),
}

/// contains the information needed by the WS Gateway to maintain the state of the WebSocket session
pub struct ClientSession<S: AsyncRead + AsyncWrite + Unpin> {
    client_id: u64,
    client_key: Option<ClientKey>,
    canister_id: Option<Principal>,
    message_for_client_tx: Option<Sender<IcWsConnectionUpdate>>,
    message_for_client_rx: Receiver<IcWsConnectionUpdate>,
    ws_write: SplitSink<WebSocketStream<S>, Message>,
    ws_read: SplitStream<WebSocketStream<S>>,
    state: IcWsSessionState,
    client_connection_span: Span,
}

impl<S: AsyncRead + AsyncWrite + Unpin> ClientSession<S> {
    pub async fn init(
        client_id: u64,
        gateway_principal: Principal,
        message_for_client_tx: Sender<IcWsConnectionUpdate>,
        message_for_client_rx: Receiver<IcWsConnectionUpdate>,
        ws_write: SplitSink<WebSocketStream<S>, Message>,
        ws_read: SplitStream<WebSocketStream<S>>,
        client_connection_span: Span,
    ) -> Result<Self, IcWsError> {
        let mut client_session = Self {
            client_id,
            client_key: None,
            canister_id: None,
            message_for_client_tx: Some(message_for_client_tx),
            message_for_client_rx,
            ws_write,
            ws_read,
            state: IcWsSessionState::Init,
            client_connection_span,
        };

        // as soon as the WS connection with the client is established, send the gateway principal
        // needed because the client doesn't know the principal of the gateway it is connecting to but only it's IP
        // however, the client has to tell the canister CDK which principal is authorized to poll its updates from the canister queue,
        // the returned principal will be included by the client in the first envelope it sends via WS

        let handshake_message = GatewayHandshakeMessage { gateway_principal };

        if let Err(e) = client_session
            .send_ws_message_to_client(Message::Binary(
                serialize(handshake_message).expect("Principal should be serializable"),
            ))
            .await
        {
            return Err(IcWsError::WebSocket(format!(
                "Error sending handshake message to client: {:?}",
                e
            )));
        }

        Ok(client_session)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> ClientSession<S> {
    pub async fn update_state(&mut self) -> Result<Option<IcWsSessionState>, IcWsError> {
        let old_state = self.state.clone();
        select! {
            // wait for incoming message from client
            Some(Ok(ws_message)) = self.ws_read.next() => {
                match self.state {
                    IcWsSessionState::Init => {
                        match self.inspect_ic_ws_open_message(ws_message.clone()).await {
                            // if the IC WS connection is setup, create a new client session and send it to the main task
                            Ok((client_key, canister_id)) => {
                                // replace the field with the canister_id received in the first envelope
                                // this shall not be updated anymore
                                // if canister_id is already set in the struct, we return an error as inspect_ic_ws_open_message shall only be called once
                                if !self.canister_id.replace(canister_id).is_none() || !self.client_key.replace(client_key).is_none() {
                                    return Err(IcWsError::IcWsProtocol(String::from(
                                        "canister_id or client_key field was set twice",
                                    )));
                                }
                                debug!("Validated WS open message");

                                // client session is now Setup
                                self.state = IcWsSessionState::Setup(
                                    // TODO: figure out if it's possible to return them as references
                                    self.client_key.clone().expect("must be set"),
                                    self.canister_id.expect("must be set"),
                                    self.message_for_client_tx.take().expect("must be set"),
                                );
                            }
                            // in case of other errors, we report them and terminate the connection handler task
                            Err(e) => {
                                return Err(IcWsError::IcWsProtocol(format!("IC WS setup failed. Error: {:?}", e)));
                            }
                        }
                    },
                    _ => unimplemented!("TODO")
                }
            },
            _ = self.message_for_client_rx.recv() => unimplemented!("TODO")
        }
        if self.state != old_state {
            Ok(Some(self.state.clone()))
        } else {
            Ok(None)
        }
    }

    async fn send_ws_message_to_client(&mut self, message: Message) -> Result<(), IcWsError> {
        if let Err(e) = self.ws_write.send(message).await {
            return Err(IcWsError::WebSocket(e.to_string()));
        }
        Ok(())
    }

    async fn inspect_ic_ws_open_message(
        &mut self,
        ws_message: Message,
    ) -> Result<(ClientKey, Principal), IcWsError> {
        let client_request = get_client_request(ws_message)?;
        // the first envelope shall have content of variant Call, which contains canister_id
        if let EnvelopeContent::Call {
            canister_id, arg, ..
        } = &*client_request.envelope.content
        {
            let (ws_open_arguments,): (CanisterWsOpenArguments,) =
                decode_args(arg).map_err(|e| {
                    IcWsError::IcWsProtocol(format!(
                        "arg field of envelope's content has the wrong type: {:?}",
                        e.to_string()
                    ))
                })?;

            let client_principal = client_request.envelope.content.sender().to_owned();
            let client_key = ClientKey::new(client_principal, ws_open_arguments.client_nonce);

            return Ok((client_key, canister_id.to_owned()));
        }
        Err(IcWsError::IcWsProtocol(String::from(
            "first message from client should contain canister_id and arg in envelope's content and should be of Call variant",
        )))
    }
}

pub struct ClientSessionHandler {
    id: ClientId,
    agent: Arc<Agent>,
    gateway_state: GatewayState,
    events_channel_tx: Sender<Box<dyn Events + Send>>,
    token: CancellationToken,
    // the client tells which canister it wants to connect to in the first envelope it sends via WS
}

impl ClientSessionHandler {
    pub fn new(
        id: ClientId,
        agent: Arc<Agent>,
        gateway_state: GatewayState,
        events_channel_tx: Sender<Box<dyn Events + Send>>,
        token: CancellationToken,
    ) -> Self {
        Self {
            id,
            agent,
            gateway_state,
            events_channel_tx,
            token,
        }
    }

    pub async fn start_session<S: AsyncRead + AsyncWrite + Unpin>(&mut self, stream: S) {
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

                let (ws_write, ws_read) = ws_stream.split();

                // [client connection handler task]        [poller task]
                // message_for_client_rx            <----- message_for_client_tx

                // channel used by the poller task to send canister messages from the directly to this client connection handler task
                // which will then forward it to the client via the WebSocket connection
                let (message_for_client_tx, message_for_client_rx): (
                    Sender<IcWsConnectionUpdate>,
                    Receiver<IcWsConnectionUpdate>,
                ) = mpsc::channel(100);

                let client_session = ClientSession::init(
                    self.id,
                    self.agent.get_principal().expect("Principal should be set"),
                    message_for_client_tx,
                    message_for_client_rx,
                    ws_write,
                    ws_read,
                    Span::current(),
                )
                .await;

                match client_session {
                    Ok(client_session) => {
                        trace!("Client session initialized");
                        self.maintain_client_session(client_session).await;
                    },
                    Err(e) => {
                        error!("Error initializing client session: {:?}", e);
                    },
                }
            },
            // no cleanup needed on the WS Gateway has the client's session has never been created
            Err(e) => {
                info!("Refused WebSocket connection {:?}", e);
            },
        }
        debug!("Terminated client connection handler task");
    }

    async fn maintain_client_session<S: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        mut client_session: ClientSession<S>,
    ) {
        loop {
            match client_session.update_state().await {
                Ok(Some(IcWsSessionState::Setup(
                    client_key,
                    canister_id,
                    message_for_client_tx,
                ))) => {
                    trace!("Client session setup");

                    // TODO: figure out if this is actually atomic
                    if let Some(poller_state) = match self.gateway_state.entry(canister_id) {
                        Entry::Occupied(mut entry) => {
                            // the poller has already been started
                            // add client key and sender end of the channel to the poller state
                            let poller_state = entry.get_mut();
                            poller_state.insert(client_key, message_for_client_tx);
                            None
                        },
                        Entry::Vacant(entry) => {
                            // the poller has not been started yet
                            // initialize the poller state and add client key and sender end of the channel
                            let poller_state =
                                Arc::new(DashMap::with_capacity_and_shard_amount(1024, 1024));
                            poller_state.insert(client_key, message_for_client_tx);
                            entry.insert(Arc::clone(&poller_state));
                            Some(Arc::clone(&poller_state))
                        },
                    } {
                        info!("Starting poller");

                        let agent = Arc::clone(&self.agent);
                        // spawn new canister poller task
                        tokio::spawn(async move {
                            let mut poller = CanisterPoller::new(canister_id, poller_state, agent);
                            poller.run_polling().await;
                        });
                    }
                },
                Err(e) => {
                    error!("Client session error: {:?}", e);
                    break;
                },
                Ok(None) => continue,
                _ => unimplemented!("TODO"),
            }
        }
    }
    // let wait_for_cancellation = self.token.cancelled();
    // tokio::pin!(wait_for_cancellation);
    // let mut ic_websocket_setup = false;
    // 'handler_loop: loop {
    //     select! {
    //         // bias select! to check token cancellation first
    //         // with 'biased', async functions are polled in the order in which they appear
    //         biased;
    //         // waits for the token to be cancelled
    //         _ = &mut wait_for_cancellation => {
    //             let graceful_shutdown_span = span!(parent: &Span::current(), Level::DEBUG, "graceful_shutdown");
    //             // close the WebSocket connection
    //             if let Err(e) = ws_write.close().await {
    //                 graceful_shutdown_span.in_scope(|| {
    //                     error!("Error closing the WS connection: {:?}", e);
    //                 });
    //             }
    //             graceful_shutdown_span.in_scope(|| {
    //                 debug!("Terminating client connection handler task due to graceful shutdown");
    //             });

    //             // TODO: update the gateway state to reflect that the client has disconnected

    //             // self.send_connection_state_to_clients_manager(
    //             //     IcWsSessionState::Closed(
    //             //         (
    //             //             self.key.clone().expect("must be some by now"),
    //             //             self.canister_id
    //             //                 .read()
    //             //                 .await
    //             //                 .expect("must be some by now")
    //             //                 .clone(),
    //             //             graceful_shutdown_span
    //             //         )
    //             //     )
    //             // )
    //             // .await;
    //             break 'handler_loop;
    //         },
    //         // wait for canister message to send to client
    //         Some(poller_message) = message_for_client_rx.recv() => {
    //             match poller_message {
    //                 // check if the poller task detected an error from the CDK
    //                 IcWsConnectionUpdate::Message((canister_message, parent_span)) => {
    //                     let message_for_client_span = span!(parent: &parent_span, Level::TRACE, "message_for_client");
    //                     let message_key = MessageReference::new(
    //                         self.canister_id
    //                             .read()
    //                             .await
    //                             .expect("must be some by now")
    //                             .clone(),
    //                         get_nonce_from_message(&canister_message.key).expect("poller relayed a message not correcly formatted")
    //                     );
    //                     let mut outgoing_canister_message_events = OutgoingCanisterMessageEvents::new(Some(EventsReference::MessageReference(message_key)), EventsCollectionType::CanisterMessage, OutgoingCanisterMessageEventsMetrics::default());
    //                     outgoing_canister_message_events.metrics.set_received_canister_message();
    //                     // relay canister message to client, cbor encoded
    //                     match to_vec(&canister_message) {
    //                         Ok(bytes) => {
    //                             match send_ws_message_to_client(&mut ws_write, Message::Binary(bytes)).await {
    //                                 Ok(_) => {
    //                                     outgoing_canister_message_events.metrics.set_message_sent_to_client();
    //                                     message_for_client_span.in_scope(|| {
    //                                         trace!("Message sent to client");
    //                                     });
    //                                 }
    //                                 Err(e) => {
    //                                     message_for_client_span.in_scope(|| {
    //                                         outgoing_canister_message_events.metrics.set_message_sent_to_client();
    //                                         error!("Could not send message to client. Error: {:?}", e);
    //                                     });
    //                                 }
    //                             }
    //                         },
    //                         Err(e) => {
    //                             outgoing_canister_message_events.metrics.set_no_message_sent_to_client();
    //                             message_for_client_span.in_scope(|| {
    //                                 error!("Could not serialize canister message. Error: {:?}", e);
    //                             });
    //                         }
    //                     }
    //                     self.events_channel_tx.send(Box::new(outgoing_canister_message_events)).await.expect("analyzer's side of the channel dropped");
    //                 },
    //                 // the poller task terminates all the client connection tasks connected to that poller
    //                 IcWsConnectionUpdate::Error(e) => {
    //                     let poller_error_span = span!(parent: &Span::current(), Level::DEBUG, "poller_error");
    //                     // close the WebSocket connection
    //                     if let Err(e) = ws_write.close().await {
    //                         poller_error_span.in_scope(|| {
    //                             error!("Error closing the WS connection: {:?}", e);
    //                         });
    //                     }
    //                     poller_error_span.in_scope(|| {
    //                         error!("Terminating client connection handler task. Error: {}", e);
    //                     });
    //                     break 'handler_loop;
    //                 }
    //             }
    //         },
    //         // wait for incoming message from client
    //         ws_message = ws_read.try_next() => {
    //             match ws_message {
    //                 // handle message sent from client via WebSocket
    //                 Ok(Some(message)) => {
    //                     // check if the WebSocket connection is closed
    //                     if message.is_close() {
    //                         let client_disconnection_span = span!(parent: &Span::current(), Level::DEBUG, "client_disconnection");
    //                         client_disconnection_span.in_scope(|| {
    //                             debug!("Terminating client connection handler task due to client disconnection");
    //                         });

    //                         // let the main task know that it should remove the client's session from the WS Gateway state
    //                         self.send_connection_state_to_clients_manager(
    //                             IcWsSessionState::Closed(
    //                                 (
    //                                     self.key.clone().expect("must be some by now"),
    //                                     self.canister_id
    //                                         .read()
    //                                         .await
    //                                         .expect("must be some by now")
    //                                         .clone(),
    //                                     client_disconnection_span
    //                                 ),
    //                             )
    //                         )
    //                         .await;
    //                         break 'handler_loop;
    //                     }
    //                     // check if the IC WebSocket connection hasn't been established yet
    //                     if !ic_websocket_setup {
    //                         match self.inspect_ic_ws_open_message(message.clone()).await {
    //                             // if the IC WS connection is setup, create a new client session and send it to the main task
    //                             Ok(IcWsSessionState::Requested(client_key)) => {
    //                                 ic_websocket_setup_span.in_scope(|| {
    //                                     debug!("Validated WS open message");
    //                                 });

    //                                 ic_websocket_setup = true;

    //                                 self.key = Some(client_key.clone());
    //                                 let client_session = ClientSession::new(
    //                                     self.id,    // used as reference for events metrics of connection establishment in gateway server
    //                                     client_key, // used to identify the client in poller
    //                                     self.canister_id    // used to specify which canister the client wants to connect to
    //                                         .read()
    //                                         .await
    //                                         .expect("must be some by now")
    //                                         .clone(),
    //                                     message_for_client_tx.clone(),  // used to send canister updates from the demux to the client connection handler
    //                                     Span::current(),
    //                                 );

    //                                 let setup_connection_async = async {
    //                                     self.send_connection_state_to_clients_manager(IcWsSessionState::Setup(
    //                                         client_session,
    //                                     ))
    //                                     .await;
    //                                     // at this point we are NOT guaranteed that the gateway server received the client session

    //                                     // if the poller is already running, it might receive the first canister message before it receives the channel from the gateway server.
    //                                     // relaying the request to the IC after sending the client session to gateway server might give it enough time to send the client channel to the poller
    //                                     // but this is not guaranteed
    //                                     // TODO: evaluate whether it is necessary to wait until the poller receives the channel or if we can assume that
    //                                     //       time_to_relay_request_to_ic + time_to_poll_first_message >> time_to_send_channel_to_poller
    //                                     if let Err(e) = self.relay_call_request_to_ic(message).await {
    //                                         error!("Could not relay request to IC. Error: {:?}", e);
    //                                     }
    //                                 };
    //                                 setup_connection_async.instrument(ic_websocket_setup_span.clone()).await;

    //                                 request_connection_setup_events
    //                                     .metrics
    //                                     .set_ws_connection_setup();
    //                                 self.events_channel_tx
    //                                     .send(Box::new(request_connection_setup_events.clone()))
    //                                     .await
    //                                     .expect("analyzer's side of the channel dropped");
    //                             }
    //                             // in case of other errors, we report them and terminate the connection handler task
    //                             Err(e) => {
    //                                 warn!("IC WS setup failed. Error: {:?}", e);
    //                                 break 'handler_loop;
    //                             }
    //                             Ok(variant) => unreachable!("handle_ic_ws_setup should not return variant: {:?}", variant)
    //                         }
    //                     } else {
    //                         let client_message_span = span!(parent: &Span::current(), Level::TRACE, "client_message");
    //                         // relay the envelope to the IC and the response back to the client
    //                         if let Err(e) = self.handle_ws_message(message).instrument(client_message_span).await {
    //                             warn!("Handling of WebSocket message failed. Error: {:?}", e);
    //                             break 'handler_loop;
    //                         }
    //                     }
    //                 },
    //                 // in this case, client's session should have been cleaned up on the WS Gateway state already
    //                 // once the connection handler received Message::Close
    //                 // just to be sure, send the cleanup message again
    //                 // TODO: figure out if this is necessary or can be ignored
    //                 Ok(None) => {
    //                     let websocket_error_span = span!(parent: &Span::current(), Level::DEBUG, "websocket_error");
    //                     websocket_error_span.in_scope(|| {
    //                         warn!("Client WebSocket connection already closed");
    //                     });
    //                     self.send_connection_state_to_clients_manager(
    //                         IcWsSessionState::Closed(
    //                             (
    //                                 self.key.clone().expect("must be some by now"),
    //                                 self.canister_id
    //                                     .read()
    //                                     .await
    //                                     .expect("must be some by now")
    //                                     .clone(),
    //                                 websocket_error_span
    //                             )
    //                         )
    //                     )
    //                     .await;
    //                     break 'handler_loop;
    //                 },
    //                 // the client's still needs to be cleaned up so it is necessary to return the client id
    //                 Err(e) => {
    //                     let websocket_error_span = span!(parent: &Span::current(), Level::DEBUG, "websocket_error");
    //                     websocket_error_span.in_scope(|| {
    //                         warn!("Client WebSocket connection error: {:?}", e);
    //                     });
    //                     // let the main task know that it should remove the client's session from the WS Gateway state
    //                     self.send_connection_state_to_clients_manager(
    //                         IcWsSessionState::Closed(
    //                             (
    //                                 self.key.clone().expect("must be some by now"),
    //                                 self.canister_id
    //                                     .read()
    //                                     .await
    //                                     .expect("must be some by now")
    //                                     .clone(),
    //                                 websocket_error_span
    //                             )
    //                         )
    //                     )
    //                     .await;
    //                     break 'handler_loop;
    //                 }
    //             };
    //         }
    //     }
    // }

    // /// relays relays the client's request to the IC and then sends the response back to the client via WS
    // /// the caller does not need to check the state fo the response, it just relays the messages back and forth
    // async fn handle_ws_message(&self, message: Message) -> Result<(), IcWsError> {
    //     self.relay_call_request_to_ic(message).await?;
    //     Ok(())
    // }

    // /// relays the client's request to the IC only if the content of the envelope is of the Call variant
    // async fn relay_call_request_to_ic(&self, message: Message) -> Result<(), IcWsError> {
    //     let client_request = get_client_request(message)?;
    //     if let EnvelopeContent::Call { .. } = *client_request.envelope.content {
    //         let serialized_envelope = serialize(client_request.envelope)?;

    //         let canister_id = self.canister_id.read().await.expect("must be some by now");

    //         // relay the envelope to the IC
    //         self.relay_envelope_to_canister(serialized_envelope, canister_id.clone())
    //             .await
    //             .map_err(|e| IcWsError::Initialization(e.to_string()))?;

    //         // there is no need to relay the response back to the client as the response to a request to the /call enpoint is not certified by the canister
    //         // and therefore could be manufactured by the gateway

    //         trace!("Relayed serialized envelope to canister");
    //         Ok(())
    //     } else {
    //         Err(IcWsError::Initialization(String::from(
    //             "Gateway can only relay envelopes with content of Call variant",
    //         )))
    //     }
    // }

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

    // async fn send_connection_state_to_clients_manager(
    //     &self,
    //     connection_state: IcWsSessionState,
    // ) {
    //     if let Err(e) = self
    //         .client_connection_handler_tx
    //         .send(connection_state)
    //         .await
    //     {
    //         error!(
    //             "Receiver has been dropped on the clients connection manager's side. Error: {:?}",
    //             e
    //         );
    //     }
    // }
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

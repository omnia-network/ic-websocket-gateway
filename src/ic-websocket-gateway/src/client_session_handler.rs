use crate::{
    canister_poller::{CanisterPoller, IcWsCanisterUpdate},
    client_session::{ClientSession, IcWsSessionState},
    events_analyzer::{Events, EventsCollectionType, EventsReference},
    manager::GatewayState,
    metrics::client_session_handler_metrics::{
        RequestConnectionSetupEvents, RequestConnectionSetupEventsMetrics,
    },
    ws_listener::ClientId,
};
use dashmap::mapref::entry::Entry;
use futures_util::StreamExt;
use ic_agent::Agent;
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_tungstenite::accept_async;
use tracing::{debug, error, info, span, trace, warn, Instrument, Level, Span};

/// Handler of a client IC WS session
pub struct ClientSessionHandler {
    /// Identifier of the client connection
    id: ClientId,
    /// Agent used to interact with the IC
    agent: Arc<Agent>,
    /// State of the gateway
    gateway_state: GatewayState,
    /// Sender side of the channel used to send events from different components to the events analyzer
    analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
    /// Polling interval in milliseconds
    polling_interval_ms: u64,
}

impl ClientSessionHandler {
    pub fn new(
        id: ClientId,
        agent: Arc<Agent>,
        gateway_state: GatewayState,
        analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
        polling_interval_ms: u64,
    ) -> Self {
        Self {
            id,
            agent,
            gateway_state,
            analyzer_channel_tx,
            polling_interval_ms,
        }
    }

    /// Upgrades to a WebSocket connection and handles the client session
    pub async fn start_session<S: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        stream: S,
    ) -> Result<(), String> {
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
                // client_channel_rx                <----- client_channel_tx

                // channel used by the poller task to send canister messages from the directly to this client connection handler task
                // which will then forward it to the client via the WebSocket connection
                let (client_channel_tx, client_channel_rx): (
                    Sender<IcWsCanisterUpdate>,
                    Receiver<IcWsCanisterUpdate>,
                ) = mpsc::channel(100);

                let client_session = ClientSession::init(
                    self.id,
                    self.agent.get_principal().expect("Principal should be set"),
                    client_channel_tx,
                    client_channel_rx,
                    ws_write,
                    ws_read,
                    Arc::clone(&self.gateway_state),
                    Arc::clone(&self.agent),
                    Span::current(),
                )
                .await;

                match client_session {
                    Ok(client_session) => {
                        debug!("Client session initialized");
                        self.handle_client_session(client_session).await;
                        Ok(())
                    },
                    Err(e) => Err(format!("Error initializing client session: {:?}", e)),
                }
            },
            Err(e) => {
                // no cleanup needed on the WS Gateway state as the client's session has never been created
                Err(format!("Refused WebSocket connection {:?}", e))
            },
        }
    }

    async fn handle_client_session<S: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        mut client_session: ClientSession<S>,
    ) {
        loop {
            match client_session.update_state().await {
                Ok(Some(IcWsSessionState::Init)) => {
                    error!("Updating the client session state cannot result in Init");
                    break;
                },
                Ok(Some(IcWsSessionState::Setup(poller_state, canister_id))) => {
                    debug!("Client session setup");
                    if let Some(poller_state) = poller_state {
                        info!("Starting poller");

                        let agent = Arc::clone(&self.agent);
                        let analyzer_channel_tx = self.analyzer_channel_tx.clone();
                        let gateway_state = Arc::clone(&self.gateway_state);
                        let polling_interval_ms = self.polling_interval_ms;
                        // spawn new canister poller task
                        tokio::spawn(async move {
                            // we pass both the whole gateway state and the poller state for the specific canister
                            // the poller can access the poller state to determine which clients are connected
                            // without having to lock the whole gateway state
                            // the poller periodically checks whether there are clients connected in the poller state and, if not,
                            // removes the corresponding entry from the gateway state and terminates
                            // TODO: figure out if this having the poller state actually helps
                            let mut poller = CanisterPoller::new(
                                canister_id,
                                poller_state,
                                gateway_state,
                                agent,
                                analyzer_channel_tx,
                                polling_interval_ms,
                            );
                            match poller.run_polling().await {
                                Ok(()) => {
                                    info!("Canister poller terminated");
                                },
                                Err(_e) => {
                                    unimplemented!("TODO");
                                },
                            };
                        });
                    }
                },
                Ok(Some(IcWsSessionState::Open)) => {
                    debug!("Client session opened");
                },
                Ok(Some(IcWsSessionState::Closed((client_key, canister_id)))) => {
                    debug!("Client session closed");

                    // TODO: figure out if this is actually atomic
                    if let Entry::Occupied(mut entry) = self.gateway_state.entry(canister_id) {
                        let poller_state = entry.get_mut();
                        if poller_state.remove(&client_key).is_none() {
                            // as the client was connected, the poller state must contain an entry for 'client_key'
                            unreachable!("Client key not found in poller state");
                        }
                        // even if this is the last client session for the canister, do not remove the canister from the gateway state
                        // this will be done by the poller task
                    } else {
                        // the gateway state must contain an entry for 'canister_id' of the canister which the client was connected to
                        unreachable!("Canister not found in gateway state");
                    }
                    break;
                },
                Err(e) => {
                    error!("Client session error: {:?}", e);
                    break;
                },
                Ok(None) => {
                    // no state change
                    continue;
                },
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
    //         Some(poller_message) = client_channel_rx.recv() => {
    //             match poller_message {
    //                 // check if the poller task detected an error from the CDK
    //                 IcWsCanisterUpdate::Message((canister_message, parent_span)) => {
    //                     let client_channel_span = span!(parent: &parent_span, Level::TRACE, "message_for_client");
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
    //                                     client_channel_span.in_scope(|| {
    //                                         trace!("Message sent to client");
    //                                     });
    //                                 }
    //                                 Err(e) => {
    //                                     client_channel_span.in_scope(|| {
    //                                         outgoing_canister_message_events.metrics.set_message_sent_to_client();
    //                                         error!("Could not send message to client. Error: {:?}", e);
    //                                     });
    //                                 }
    //                             }
    //                         },
    //                         Err(e) => {
    //                             outgoing_canister_message_events.metrics.set_no_message_sent_to_client();
    //                             client_channel_span.in_scope(|| {
    //                                 error!("Could not serialize canister message. Error: {:?}", e);
    //                             });
    //                         }
    //                     }
    //                     self.analyzer_channel_tx.send(Box::new(outgoing_canister_message_events)).await.expect("analyzer's side of the channel dropped");
    //                 },
    //                 // the poller task terminates all the client connection tasks connected to that poller
    //                 IcWsCanisterUpdate::Error(e) => {
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
    //                                     client_channel_tx.clone(),  // used to send canister updates from the demux to the client connection handler
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
    //                                 self.analyzer_channel_tx
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

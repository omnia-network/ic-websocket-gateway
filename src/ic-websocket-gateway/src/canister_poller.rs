use crate::{
    canister_methods::{
        self, CanisterOpenMessageContent, CanisterOutputCertifiedMessages, CanisterOutputMessage,
        CanisterServiceMessage, CanisterToClientMessage, CanisterWsCloseArguments,
        CanisterWsGetMessagesArguments, ClientKey, WebsocketMessage,
    },
    events_analyzer::{
        Events, EventsCollectionType, EventsImpl, EventsReference, IterationReference,
        MessageReference,
    },
    manager::{CanisterPrincipal, ClientSender, GatewaySharedState, PollerState},
    metrics::canister_poller_metrics::{
        IncomingCanisterMessageEvents, IncomingCanisterMessageEventsMetrics, PollerEvents,
        PollerEventsMetrics,
    },
};
use candid::{decode_one, Principal};
use ic_agent::Agent;
use serde_cbor::from_slice;
use std::{
    borrow::BorrowMut,
    cell::RefCell,
    rc::Rc,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{
    select,
    sync::{
        mpsc::{Receiver, Sender},
        RwLock,
    },
    time::Instant,
};
use tracing::{debug, error, info, span, trace, warn, Id, Instrument, Level, Span};

// OLD NOTE: 30 seems to be a good value for polling interval 100 ms and incoming connection rate up to 10 per second
//           as not so many polling iterations are idle and the effective polling interval (measured by PollerEventsMetrics) is mostly in [200, 300] ms
// TODO: make sure this is always in sync with the CDK init parameter 'max_number_of_returned_messages'
//       maybe get it once starting to poll the canister (?)
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 30;

enum PollingStatus {
    NoMessagesPolled,
    MessagesPolled(CanisterOutputCertifiedMessages),
    MaxMessagesPolled(CanisterOutputCertifiedMessages),
}

enum PollerStatus {
    Running,
    Terminated,
}

/// updates the client connection handler on the IC WS connection state
pub enum IcWsCanisterUpdate {
    /// contains a new message to be realyed to the client
    Message((CanisterToClientMessage, Span)),
    /// lets the client connection hanlder know that an error occurred and the connection should be closed
    Error(String),
}

/// Poller which periodically queries a canister for new messages and relays them to the client
pub struct CanisterPoller {
    /// Agent used to communicate with the IC
    agent: Arc<Agent>,
    /// Principal of the canister which the poller is polling
    canister_id: CanisterPrincipal,
    /// State of the poller
    poller_state: PollerState,
    /// State of the gateway
    gateway_shared_state: GatewaySharedState,
    /// Nonce specified by the gateway during the query call to ws_get_messages,
    /// used by the CDK to determine which messages to respond with
    message_nonce: u64,
    /// The number of polling iterations since the poller started
    /// reference of the PollerEvents
    polling_iteration: u64,
    /// Polling interval in milliseconds
    polling_interval_ms: u64,
    /// Sender side of the channel used to send events to the analyzer
    _analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
}

impl CanisterPoller {
    pub fn new(
        agent: Arc<Agent>,
        canister_id: Principal,
        poller_state: PollerState,
        gateway_shared_state: GatewaySharedState,
        polling_interval_ms: u64,
        _analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
    ) -> Self {
        Self {
            agent,
            canister_id,
            poller_state,
            gateway_shared_state,
            // once the poller starts running, it requests messages from nonce 0.
            // if the canister already has some messages in the queue and receives the nonce 0, it knows that the poller restarted
            // therefore, it sends the last X messages to the gateway. From these, the gateway has to determine the response corresponding to the client's ws_open request
            // TODO: change the CDK so that in this case it returns all the messages in the queue
            //       the poller relays only the ones that are in the poller state at the time of receiving them
            message_nonce: 0,
            polling_iteration: 0,
            polling_interval_ms,
            _analyzer_channel_tx,
        }
    }

    /// Periodically polls the canister for updates to be relayed to clients
    pub async fn run_polling(&mut self) -> Result<(), String> {
        loop {
            let polling_iteration_span = span!(Level::TRACE, "Polling Iteration", canister_id = %self.canister_id, polling_iteration = self.polling_iteration);
            self.poll_and_relay()
                .instrument(polling_iteration_span)
                .await?;

            // counting all polling iterations (instead of only the ones that return at least one canister message)
            // this way we can tell for how many iterations the poller was "idle" before actually getting some messages from the canister
            // this can help us in the future understanding whether the poller is polling too frequently or not
            self.polling_iteration += 1;

            if let PollerStatus::Terminated = self.check_poller_termination() {
                // the poller has been terminated
                return Ok(());
            }
        }
    }

    async fn poll_and_relay(&mut self) -> Result<(), String> {
        let relay_messages_span =
            span!(parent: &Span::current(), Level::TRACE, "Relay Canister Messages");

        let start_polling_instant: tokio::time::Instant = tokio::time::Instant::now();
        match self.poll_canister().instrument(Span::current()).await {
            Ok(PollingStatus::NoMessagesPolled) => {
                // if no messages are returned, sleep for 'polling_interval_ms' before polling again
                tokio::time::sleep(Duration::from_millis(self.polling_interval_ms)).await;
                Ok(())
            },
            Ok(PollingStatus::MessagesPolled(certified_canister_output)) => {
                self.relay_messages(certified_canister_output, Span::current().id())
                    .instrument(relay_messages_span)
                    .await?;
                let elapsed = get_elapsed(start_polling_instant);
                let polling_interval = Duration::from_millis(self.polling_interval_ms);
                // check if polling and relaying took longer than 'polling_interval'
                // if yes, restart polling immediately
                // otherwise, sleep for the amount of time remaining to 'polling_interval'
                if elapsed > polling_interval {
                    warn!(
                        "Polling and relaying of messages took too long: {:?}. Polling immediately",
                        elapsed
                    );
                } else {
                    // SAFETY:
                    // 'elapsed' is smaller than 'polling_interval'
                    // therefore, the duration passed to 'sleep' is valid

                    // 'elapsed' is >= 0
                    // therefore, the next polling iteration is delayed by at most 'polling_interval'
                    tokio::time::sleep(polling_interval - elapsed).await;
                }
                Ok(())
            },
            Ok(PollingStatus::MaxMessagesPolled(certified_canister_output)) => {
                self.relay_messages(certified_canister_output, Span::current().id())
                    .instrument(relay_messages_span)
                    .await?;
                let elapsed = get_elapsed(start_polling_instant);
                // if polling and relaying took longer than 'polling_interval', create a warning
                if elapsed > Duration::from_millis(self.polling_interval_ms) {
                    warn!(
                        "Polling and relaying of messages took too long: {:?}",
                        elapsed
                    );
                }
                // poll immediately as the maximum number of messages has beeen polled
                warn!("Polled the maximum number of messages. Polling immediately");
                Ok(())
            },
            Err(e) => Err(e),
        }
    }

    /// Polls the canister for messages
    async fn poll_canister(&mut self) -> Result<PollingStatus, String> {
        trace!("Started polling iteration");

        // get messages to be relayed to clients from canister (starting from 'message_nonce')
        let certified_canister_output = canister_methods::ws_get_messages(
            &self.agent,
            &self.canister_id,
            CanisterWsGetMessagesArguments {
                nonce: self.message_nonce,
            },
        )
        .await?;

        let number_of_polled_messages = certified_canister_output.messages.len();

        if number_of_polled_messages == 0 {
            trace!("No messages polled from canister");
            Ok(PollingStatus::NoMessagesPolled)
        } else if number_of_polled_messages >= MAX_NUMBER_OF_RETURNED_MESSAGES {
            trace!("Polled the maximum number of messages");
            Ok(PollingStatus::MaxMessagesPolled(certified_canister_output))
        } else {
            trace!(
                "Polled {} messages from canister",
                number_of_polled_messages
            );
            Ok(PollingStatus::MessagesPolled(certified_canister_output))
        }
    }

    pub async fn relay_messages(
        &mut self,
        msgs: CanisterOutputCertifiedMessages,
        relay_messages_span_id: Option<Id>,
    ) -> Result<(), String> {
        for canister_output_message in msgs.messages {
            let canister_to_client_message = CanisterToClientMessage {
                key: canister_output_message.key,
                content: canister_output_message.content,
                cert: msgs.cert.clone(),
                tree: msgs.tree.clone(),
            };

            let last_message_nonce = get_nonce_from_message(&canister_to_client_message.key)?;

            // TODO: figure out if keeping references to a value in the poller state can cause deadlocks
            if let Some(ClientSender {
                sender: client_channel_tx,
                span: client_connection_span,
            }) = self
                .poller_state
                .get(&canister_output_message.client_key)
                .as_deref()
            {
                let canister_message_span = span!(parent: client_connection_span, Level::TRACE, "Canister Message", message_key = canister_to_client_message.key);
                canister_message_span.follows_from(relay_messages_span_id.clone());
                canister_message_span.in_scope(|| trace!("Received message from canister",));
                self.relay_message(canister_to_client_message, client_channel_tx)
                    .instrument(canister_message_span)
                    .await;
            } else {
                // SAFETY:
                // messages received from a client key that is not in the poller state are ignored
                // this is safe to do because we the client session handler relayes the messages to the IC
                // only after updating the poller state
                trace!("Polled message for a client that is not in the poller state anymore. Ignoring message");
            }
            self.message_nonce = last_message_nonce + 1;
        }
        trace!("Relayed messages to connection handlers");
        Ok(())
    }

    pub async fn relay_message(
        &self,
        canister_to_client_message: CanisterToClientMessage,
        client_channel_tx: &Sender<IcWsCanisterUpdate>,
    ) {
        if let Err(e) = client_channel_tx
            .send(IcWsCanisterUpdate::Message((
                canister_to_client_message,
                Span::current(),
            )))
            .await
        {
            // SAFETY:
            // no need to panic here as the client session handler might have terminated
            // after the poller got the client_chanel_tx
            // the client session handler also updated the poller state so the poller can simply ignore this message
            warn!("Client's session terminated: {}", e);
        } else {
            trace!("Message relayed to connection handler");
        }
    }

    fn check_poller_termination(&mut self) -> PollerStatus {
        // check if the poller should be terminated
        // the poller does not necessarily need to be terminated as soon as the last client disconnects
        // therefore, we do not need to check if the poller state is empty in every single polling iteration
        if self.polling_iteration % 100 == 0 {
            if self
                .gateway_shared_state
                .remove_canister_if_empty(self.canister_id)
            {
                info!("Terminating poller");
                return PollerStatus::Terminated;
            }
            return PollerStatus::Running;
        }
        PollerStatus::Running
    }
}

pub fn get_nonce_from_message(key: &String) -> Result<u64, String> {
    if let Some(message_nonce_str) = key.split('_').last() {
        let message_nonce = message_nonce_str
            .parse()
            .map_err(|e| format!("Could not parse nonce. Error: {:?}", e))?;
        return Ok(message_nonce);
    }
    Err(String::from(
        "Key in canister message is not formatted correctly",
    ))
}

fn get_elapsed(start: Instant) -> Duration {
    let now = Instant::now();
    now - start
}

// let start_new_poller_span = span!(parent: &client_connection_span, Level::DEBUG, "start_new_poller_span", canister_id = %self.canister_id);

// let messages_demux = Rc::new(RefCell::new(MessagesDemux::new(
//     poller_channels.poller_to_analyzer.clone(),
//     self.canister_id,
// )));
// // the channel used to send updates to the first client is passed as an argument to the poller
// // this way we can be sure that once the poller gets the first messages from the canister, there is already a client to send them to
// // this also ensures that we can detect which messages in the first polling iteration are "old" and which ones are not
// // this is necessary as the poller once it starts it does not know the nonce of the last message delivered by the canister
// messages_demux.borrow_mut().add_client_channel(
//     first_client_key.clone(),
//     client_channel_tx,
//     client_connection_span,
// );

// let get_canister_updates =
//     self.get_canister_updates(first_client_key.clone(), Rc::clone(&messages_demux));
// // pin the tracking of the in-flight asynchronous operation so that in each select! iteration get_canister_updates is continued
// // instead of issuing a new call to get_canister_updates
// tokio::pin!(get_canister_updates);

// start_new_poller_span.in_scope(|| {
//     info!("Started runnning canister poller");
// });
// drop(start_new_poller_span);

// 'poller_loop: loop {
//     select! {
//         // receive channel used to send canister updates to new client's task
//         Some(channel_data) = poller_channels.main_to_poller.recv() => {
//             match channel_data {
//                 PollerToClientChannelData::NewClientChannel(client_key, client_channel, client_connection_span) => {
//                     messages_demux
//                         .borrow_mut()
//                         .add_client_channel(client_key, client_channel, client_connection_span);
//                 },
//                 PollerToClientChannelData::ClientDisconnected(client_key, span) => {

//                     messages_demux
//                         .borrow_mut()
//                         .remove_client_state(&client_key, span.clone());
//                     call_ws_close_in_background(
//                         self.agent.clone(),
//                         self.canister_id,
//                         client_key,
//                         span
//                     );
//             }
//             }
//         }
//         // poll canister for updates across multiple select! iterations
//         res = &mut get_canister_updates => {
//             match res {
//                 Ok(poller_events) => {
//                     poller_channels
//                         .poller_to_analyzer
//                         .send(Box::new(poller_events))
//                         .await
//                         .expect("analyzer's side of the channel dropped");
//                 }

//                 Err(e) => {
//                     let poller_error_span = span!(Level::DEBUG, "poller_error");
//                     error!("Terminating poller task due to CDK error: {}", e);
//                     signal_termination_and_cleanup(
//                         &self.agent,
//                         &mut poller_channels.poller_to_main,
//                         self.canister_id,
//                         messages_demux,
//                         e,
//                     ).instrument(poller_error_span)
//                     .await;
//                     break 'poller_loop;
//                 }
//             }

//             // counting all polling iterations (instead of only the ones that return at least one canister message)
//             // this way we can tell for how many iterations the poller was "idle" before actually getting some messages from the canister
//             // this can help us in the future understanding whether the poller is polling too frequently or not
//             *self.polling_iteration.borrow_mut() += 1;

//             // pin a new asynchronous operation so that it can be restarted in the next select! iteration and continued in the following ones
//             get_canister_updates.set(self.get_canister_updates(first_client_key.clone(), Arc::clone(&messages_demux)));
//         },
//     }
//     // exit task if last client disconnected
//     if messages_demux.borrow().count_registered_clients() == 0 {
//         info!("Terminating poller task as no clients are connected");
//         signal_poller_task_termination(
//             &mut poller_channels.poller_to_main,
//             TerminationInfo::LastClientDisconnected(self.canister_id),
//         )
//         .await;
//         break;
//     }
//     // prevents the poller from blocking the thread
//     // TODO: figure out if it is necessary
//     tokio::task::yield_now().await;
// }

// async fn signal_termination_and_cleanup(
//     agent: &Arc<Agent>,
//     poller_to_main_channel: &mut Sender<TerminationInfo>,
//     canister_id: Principal,
//     messages_demux: Rc<RefCell<MessagesDemux>>,
//     e: String,
// ) {
//     // let the main task know that this poller will terminate due to a CDK error
//     signal_poller_task_termination(
//         poller_to_main_channel,
//         TerminationInfo::CdkError(canister_id),
//     )
//     .await;
//     // let each client connection handler task connected to this poller know that the poller will terminate
//     // and thus they also have to close the WebSocket connection and terminate
//     for (client_key, (client_channel_tx, _parent_span)) in messages_demux.borrow().client_channels()
//     {
//         call_ws_close_in_background(
//             agent.clone(),
//             canister_id,
//             client_key.to_owned(),
//             Span::current(),
//         );
//         if let Err(channel_err) = client_channel_tx
//             .send(IcWsCanisterUpdate::Error(format!(
//                 "Terminating poller task due to error: {}",
//                 e
//             )))
//             .await
//         {
//             error!("Client's thread terminated: {}", channel_err);
//         }
//     }
// }

// async fn signal_poller_task_termination(
//     channel: &mut Sender<TerminationInfo>,
//     info: TerminationInfo,
// ) {
//     if let Err(e) = channel.send(info).await {
//         error!(
//             "Receiver has been dropped on the pollers connection manager's side. Error: {:?}",
//             e
//         );
//     }
// }

async fn sleep(millis: u64) {
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

// fn call_ws_close_in_background(
//     agent: Arc<Agent>,
//     canister_id: Principal,
//     client_key: ClientKey,
//     span: Span,
// ) {
//     // close client connection on canister
//     // sending the request to the canister takes a few seconds
//     // therefore this is done in a separate task
//     // in order to not slow down the poller task
//     tokio::spawn(
//         async move {
//             debug!("Calling ws_close for client");
//             // TODO: figure out why it takes 10-30 seconds for the canister to close the connection
//             if let Err(e) = canister_methods::ws_close(
//                 &agent,
//                 &canister_id,
//                 CanisterWsCloseArguments { client_key },
//             )
//             .await
//             {
//                 error!("Calling ws_close on canister failed: {}", e);
//             } else {
//                 debug!("Canister closed connection with client");
//             }
//             CLIENTS_REGISTERED_IN_CDK.fetch_sub(1, Ordering::SeqCst);
//         }
//         .instrument(span),
//     );
// }

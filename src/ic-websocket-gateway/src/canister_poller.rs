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
    manager::PollerState,
    messages_demux::CLIENTS_REGISTERED_IN_CDK,
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
};
use tracing::{debug, error, info, span, trace, warn, Id, Instrument, Level, Span};

// TODO: make sure this is always in sync with the CDK init parameter 'max_number_of_returned_messages'
//       maybe get it once starting to poll the canister (?)
//       30 seems to be a good value for polling interval 100 ms and incoming connection rate up to 10 per second
//       as not so many polling iterations are idle and the effective polling interval (measured by PollerEventsMetrics) is mostly in [200, 300] ms
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 30;

/// ends of the channels needed by each canister poller tasks
#[derive(Debug)]
pub struct PollerChannelsPollerEnds {
    /// receiving side of the channel used by the main task to send the receiving task of a new client's channel to the poller task
    main_to_poller: Receiver<PollerToClientChannelData>,
    /// sending side of the channel used by the poller to send the canister id of the poller which is about to terminate
    poller_to_main: Sender<TerminationInfo>,
    /// sending side of the channel used by the poller to send events to the event analyzer
    poller_to_analyzer: Sender<Box<dyn Events + Send>>,
}

impl PollerChannelsPollerEnds {
    pub fn new(
        main_to_poller: Receiver<PollerToClientChannelData>,
        poller_to_main: Sender<TerminationInfo>,
        poller_to_analyzer: Sender<Box<dyn Events + Send>>,
    ) -> Self {
        Self {
            main_to_poller,
            poller_to_main,
            poller_to_analyzer,
        }
    }
}

/// updates the client connection handler on the IC WS connection state
pub enum IcWsConnectionUpdate {
    /// contains a new message to be realyed to the client
    Message((CanisterToClientMessage, Span)),
    /// lets the client connection hanlder know that an error occurred and the connection should be closed
    Error(String),
}

/// contains the information that the main task sends to the poller task:
#[derive(Debug, Clone)]
pub enum PollerToClientChannelData {
    /// contains the sending side of the channel use by the poller to send messages to the client
    NewClientChannel(ClientKey, Sender<IcWsConnectionUpdate>, Span),
    /// signals the poller which cllient disconnected
    ClientDisconnected(ClientKey, Span),
}

/// determines the reason of the poller task termination:
pub enum TerminationInfo {
    /// contains the principal of the last client and therefore there is no need to continue polling
    LastClientDisconnected(Principal),
    /// error while polling the canister
    CdkError(Principal),
}

/// periodically polls the canister for updates to be relayed to clients
pub struct CanisterPoller {
    canister_id: Principal,
    poller_state: PollerState,
    /// nonce specified by the gateway during the query call to ws_get_messages, used by the CDK to determine which messages to send
    message_nonce: u64,
    /// reference of the PollerEvents
    polling_iteration: u64,
    agent: Arc<Agent>,
    polling_interval_ms: u64,
    analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
}

impl CanisterPoller {
    pub fn new(
        canister_id: Principal,
        poller_state: PollerState,
        agent: Arc<Agent>,
        analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
    ) -> Self {
        Self {
            canister_id,
            poller_state,
            // once the poller starts running, it requests messages from nonce 0.
            // if the canister already has some messages in the queue and receives the nonce 0, it knows that the poller restarted
            // therefore, it sends the last X messages to the gateway. From these, the gateway has to determine the response corresponding to the client's ws_open request
            message_nonce: 0,
            polling_iteration: 0,
            agent,
            // TODO: let the canister be able to configure the polling interval
            polling_interval_ms: 100,
            analyzer_channel_tx,
        }
    }

    pub async fn run_polling(&mut self) {
        loop {
            info!("Polling canister");
            self.poll_canister().await.unwrap();

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn poll_canister(&mut self) -> Result<PollerEvents, String> {
        let polling_iteration_span =
            span!(Level::TRACE, "Polling Iteration", canister_id = %self.canister_id);

        polling_iteration_span.in_scope(|| trace!("Started polling iteration"));

        let iteration_key = IterationReference::new(self.canister_id, self.polling_iteration);
        let mut poller_events = PollerEvents::new(
            Some(EventsReference::IterationReference(iteration_key.clone())),
            EventsCollectionType::PollerStatus,
            PollerEventsMetrics::default(),
        );
        poller_events.metrics.set_start_polling();

        // get messages to be relayed to clients from canister (starting from 'message_nonce')
        let certified_canister_output = canister_methods::ws_get_messages(
            &self.agent,
            &self.canister_id,
            CanisterWsGetMessagesArguments {
                nonce: self.message_nonce,
            },
        )
        .await?;
        polling_iteration_span.in_scope(|| trace!("Polled canister for messages"));

        poller_events.metrics.set_received_messages();

        let polling_interval_ms = self.polling_interval_ms;

        let number_of_returned_messages = certified_canister_output.messages.len();
        if number_of_returned_messages > 0 {
            let polled_messages_span = span!(parent: &polling_iteration_span, Level::TRACE, "polled_messages", iteration_key = %iteration_key);

            let relay_messages_async = async {
                polled_messages_span.in_scope(|| {
                    trace!(
                        "Polled {} messages from canister",
                        number_of_returned_messages
                    )
                });

                poller_events.metrics.set_start_relaying_messages();

                if let Err(e) = self
                    .relay_messages(certified_canister_output, polled_messages_span.id())
                    .await
                {
                    return Err(format!("Relaying messages failed: {:?}", e));
                }
                polled_messages_span.in_scope(|| trace!("Relayed messages to connection handlers"));
                Ok(())
            };

            if number_of_returned_messages >= MAX_NUMBER_OF_RETURNED_MESSAGES {
                polled_messages_span.in_scope(|| {
                    warn!("Polled the maximum number of messages. Relayng messages and polling again with no delay");
                });
                relay_messages_async.await?;
                polling_iteration_span.in_scope(|| trace!("Finished polling iteration"));
            } else {
                tokio::join!(relay_messages_async, sleep(polling_interval_ms)).0?;
            }
        } else {
            polling_iteration_span.in_scope(|| trace!("No messages polled"));
        }

        // counting all polling iterations (instead of only the ones that return at least one canister message)
        // this way we can tell for how many iterations the poller was "idle" before actually getting some messages from the canister
        // this can help us in the future understanding whether the poller is polling too frequently or not
        self.polling_iteration += 1;
        poller_events.metrics.set_finished_relaying_messages();
        Ok(poller_events)
    }

    pub async fn relay_messages(
        &mut self,
        msgs: CanisterOutputCertifiedMessages,
        polled_messages_span_id: Option<Id>,
    ) -> Result<(), String> {
        for canister_output_message in msgs.messages {
            let canister_to_client_message = CanisterToClientMessage {
                key: canister_output_message.key,
                content: canister_output_message.content,
                cert: msgs.cert.clone(),
                tree: msgs.tree.clone(),
            };
            let mut incoming_canister_message_events = IncomingCanisterMessageEvents::new(
                None,
                EventsCollectionType::CanisterMessage,
                IncomingCanisterMessageEventsMetrics::default(),
            );
            incoming_canister_message_events
                .metrics
                .set_start_relaying_message();

            let last_message_nonce = get_nonce_from_message(&canister_to_client_message.key)?;
            let message_key = MessageReference::new(self.canister_id, last_message_nonce.clone());
            incoming_canister_message_events.reference =
                Some(EventsReference::MessageReference(message_key));

            // SAFETY:
            // messages received from a client key that is not in the poller state are ignored
            // this is safe to do because we the client session handler relayes the messages to the IC
            // only after updating the poller state

            // TODO: figure out if keeping references to a value in the poller state can cause deadlocks
            if let Some((message_for_client_tx, client_connection_span)) = self
                .poller_state
                .get(&canister_output_message.client_key)
                .as_deref()
            {
                let canister_message_span = span!(parent: client_connection_span, Level::TRACE, "canister_message", message_key = canister_to_client_message.key);
                canister_message_span.follows_from(polled_messages_span_id.clone());
                canister_message_span.in_scope(|| trace!("Received message from canister",));
                self.relay_message(
                    canister_to_client_message,
                    message_for_client_tx,
                    incoming_canister_message_events,
                )
                .instrument(canister_message_span)
                .await;
            }
            // TODO: check without panicking
            // assert_eq!(*message_nonce, last_message_nonce); // check that messages are relayed in increasing order

            self.message_nonce = last_message_nonce + 1;
        }
        Ok(())
    }

    pub async fn relay_message(
        &self,
        canister_to_client_message: CanisterToClientMessage,
        message_for_client_tx: &Sender<IcWsConnectionUpdate>,
        mut incoming_canister_message_events: EventsImpl<IncomingCanisterMessageEventsMetrics>,
    ) {
        if let Err(e) = message_for_client_tx
            .send(IcWsConnectionUpdate::Message((
                canister_to_client_message,
                Span::current(),
            )))
            .await
        {
            error!("Client's session terminated: {}", e);
            incoming_canister_message_events
                .metrics
                .set_no_message_relayed();
        } else {
            incoming_canister_message_events
                .metrics
                .set_message_relayed();
            trace!("Message relayed to connection handler");
        }
        self.analyzer_channel_tx
            .send(Box::new(incoming_canister_message_events))
            .await
            .expect("analyzer's side of the channel dropped");
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
//     message_for_client_tx,
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
//             .send(IcWsConnectionUpdate::Error(format!(
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

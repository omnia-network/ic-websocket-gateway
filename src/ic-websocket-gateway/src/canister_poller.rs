use crate::{
    canister_methods::{
        self, CanisterOpenMessageContent, CanisterOutputMessage, CanisterServiceMessage,
        CanisterToClientMessage, CanisterWsCloseArguments, CanisterWsGetMessagesArguments,
        ClientKey, WebsocketMessage,
    },
    events_analyzer::{Events, EventsCollectionType, EventsReference, IterationReference},
    messages_demux::{MessagesDemux, CLIENTS_REGISTERED_IN_CDK},
    metrics::canister_poller_metrics::{PollerEvents, PollerEventsMetrics},
};
use candid::{decode_one, Principal};
use ic_agent::Agent;
use serde_cbor::from_slice;
use std::{
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
use tracing::{debug, error, info, span, trace, warn, Instrument, Level, Span};

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
    /// nonce specified by the gateway during the query call to ws_get_messages, used by the CDK to determine which messages to send
    message_nonce: Arc<RwLock<u64>>,
    /// reference of the PollerEvents
    polling_iteration: Arc<RwLock<u64>>,
    agent: Arc<Agent>,
    polling_interval_ms: u64,
}

impl CanisterPoller {
    pub fn new(canister_id: Principal, agent: Arc<Agent>, polling_interval_ms: u64) -> Self {
        Self {
            canister_id,
            // once the poller starts running, it requests messages from nonce 0.
            // if the canister already has some messages in the queue and receives the nonce 0, it knows that the poller restarted
            // therefore, it sends the last X messages to the gateway. From these, the gateway has to determine the response corresponding to the client's ws_open request
            message_nonce: Arc::new(RwLock::new(0)),
            polling_iteration: Arc::new(RwLock::new(0)),
            agent,
            polling_interval_ms,
        }
    }

    pub async fn run_polling(
        &mut self,
        mut poller_channels: PollerChannelsPollerEnds,
        first_client_key: ClientKey,
        message_for_client_tx: Sender<IcWsConnectionUpdate>,
        client_connection_span: Span,
    ) {
        let start_new_poller_span = span!(parent: &client_connection_span, Level::DEBUG, "start_new_poller_span", canister_id = %self.canister_id);

        let messages_demux = Arc::new(RwLock::new(MessagesDemux::new(
            poller_channels.poller_to_analyzer.clone(),
            self.canister_id,
        )));
        // the channel used to send updates to the first client is passed as an argument to the poller
        // this way we can be sure that once the poller gets the first messages from the canister, there is already a client to send them to
        // this also ensures that we can detect which messages in the first polling iteration are "old" and which ones are not
        // this is necessary as the poller once it starts it does not know the nonce of the last message delivered by the canister
        messages_demux.write().await.add_client_channel(
            first_client_key.clone(),
            message_for_client_tx,
            client_connection_span,
        );

        let get_canister_updates =
            self.get_canister_updates(first_client_key.clone(), Arc::clone(&messages_demux));
        // pin the tracking of the in-flight asynchronous operation so that in each select! iteration get_canister_updates is continued
        // instead of issuing a new call to get_canister_updates
        tokio::pin!(get_canister_updates);

        start_new_poller_span.in_scope(|| {
            info!("Started runnning canister poller");
        });
        drop(start_new_poller_span);

        'poller_loop: loop {
            select! {
                // receive channel used to send canister updates to new client's task
                Some(channel_data) = poller_channels.main_to_poller.recv() => {
                    match channel_data {
                        PollerToClientChannelData::NewClientChannel(client_key, client_channel, client_connection_span) => {
                            messages_demux
                                .write()
                                .await
                                .add_client_channel(client_key, client_channel, client_connection_span);
                        },
                        PollerToClientChannelData::ClientDisconnected(client_key, span) => {
                            messages_demux
                                .write()
                                .await
                                .remove_client_state(&client_key, span.clone());
                            call_ws_close_in_background(
                                self.agent.clone(),
                                self.canister_id,
                                client_key,
                                span
                            );
                    }
                    }
                }
                // poll canister for updates across multiple select! iterations
                res = &mut get_canister_updates => {
                    match res {
                        Ok(poller_events) => {
                            poller_channels
                                .poller_to_analyzer
                                .send(Box::new(poller_events))
                                .await
                                .expect("analyzer's side of the channel dropped");
                        }

                        Err(e) => {
                            let poller_error_span = span!(Level::DEBUG, "poller_error");
                            error!("Terminating poller task due to CDK error: {}", e);
                            signal_termination_and_cleanup(
                                &self.agent,
                                &mut poller_channels.poller_to_main,
                                self.canister_id,
                                messages_demux,
                                e,
                            ).instrument(poller_error_span)
                            .await;
                            break 'poller_loop;
                        }
                    }

                    // counting all polling iterations (instead of only the ones that return at least one canister message)
                    // this way we can tell for how many iterations the poller was "idle" before actually getting some messages from the canister
                    // this can help us in the future understanding whether the poller is polling too frequently or not
                    *self.polling_iteration.write().await += 1;

                    // pin a new asynchronous operation so that it can be restarted in the next select! iteration and continued in the following ones
                    get_canister_updates.set(self.get_canister_updates(first_client_key.clone(), Arc::clone(&messages_demux)));
                },
            }
            // exit task if last client disconnected
            if messages_demux.read().await.count_registered_clients() == 0 {
                info!("Terminating poller task as no clients are connected");
                signal_poller_task_termination(
                    &mut poller_channels.poller_to_main,
                    TerminationInfo::LastClientDisconnected(self.canister_id),
                )
                .await;
                break;
            }
            // prevents the poller from blocking the thread
            // TODO: figure out if it is necessary
            tokio::task::yield_now().await;
        }
    }

    async fn get_canister_updates(
        &self,
        first_client_key: ClientKey,
        messages_demux: Arc<RwLock<MessagesDemux>>,
    ) -> Result<PollerEvents, String> {
        let polling_iteration_span =
            span!(Level::TRACE, "Polling Iteration", canister_id = %self.canister_id);
        polling_iteration_span.in_scope(|| trace!("Started polling iteration"));
        let iteration_key =
            IterationReference::new(self.canister_id, *self.polling_iteration.read().await);
        let mut poller_events = PollerEvents::new(
            Some(EventsReference::IterationReference(iteration_key.clone())),
            EventsCollectionType::PollerStatus,
            PollerEventsMetrics::default(),
        );
        poller_events.metrics.set_start_polling();

        // get messages to be relayed to clients from canister (starting from 'message_nonce')
        let mut certified_canister_output = canister_methods::ws_get_messages(
            &self.agent,
            &self.canister_id,
            CanisterWsGetMessagesArguments {
                nonce: *self.message_nonce.read().await,
            },
        )
        .await?;
        polling_iteration_span.in_scope(|| trace!("Polled canister for messages"));

        poller_events.metrics.set_received_messages();

        let relay_messages_async = async {
            // process messages in queues before the ones just polled from the canister (if any) so that the clients receive messages in the expected order
            // this is done even if no messages are returned from the current polling iteration as there might be messages in the queue waiting to be processed
            messages_demux
                .write()
                .await
                .process_queues(polling_iteration_span.id())
                .await;

            filter_canister_messages(
                &mut certified_canister_output.messages,
                *self.message_nonce.read().await,
                first_client_key,
            );

            let number_of_returned_messages = certified_canister_output.messages.len();
            if number_of_returned_messages > 0 {
                let polled_messages_span = span!(parent: &polling_iteration_span, Level::TRACE, "polled_messages", iteration_key = %iteration_key);
                polled_messages_span.in_scope(|| {
                    trace!(
                        "Polled {} messages from canister",
                        number_of_returned_messages
                    )
                });
                if number_of_returned_messages >= MAX_NUMBER_OF_RETURNED_MESSAGES {
                    polled_messages_span.in_scope(|| {
                        warn!("Consider increasing the polling frequency or the maximum number of returned messages by the CDK");
                    });
                };

                poller_events.metrics.set_start_relaying_messages();
                if let Err(e) = messages_demux
                    .write()
                    .await
                    .relay_messages(
                        certified_canister_output,
                        self.message_nonce.clone(),
                        polled_messages_span.id(),
                    )
                    .await
                {
                    return Err(e);
                }
                polled_messages_span.in_scope(|| trace!("Relayed messages to connection handlers"));
                drop(polled_messages_span);
            } else {
                polling_iteration_span.in_scope(|| trace!("No messages polled"));
            }
            poller_events.metrics.set_finished_relaying_messages();
            Ok(poller_events)
        };

        let (relay_result, _) = tokio::join!(relay_messages_async, sleep(self.polling_interval_ms));
        polling_iteration_span.in_scope(|| trace!("Finished polling iteration"));
        drop(polling_iteration_span);
        relay_result
    }
}

pub fn filter_canister_messages<'a>(
    messages: &'a mut Vec<CanisterOutputMessage>,
    message_nonce: u64,
    first_client_key: ClientKey,
) {
    if message_nonce == 0 {
        // if the poller just started (message_nonce == 0), the canister might have already had other messages in the queue which we should not send to the clients
        // therefore, starting from the last message polled, we relay the open message of type CanisterServiceMessage::OpenMessage for each connected client
        // message_nonce has to be set to the nonce of the last open message pollled in this iteration so that in the next iteration we can poll from there
        filter_messages_of_first_polling_iteration(messages, first_client_key);
    }
    // this is not the first polling iteration and therefore the poller queried the canister starting from the nonce of the last message of the previous polling iteration
    // therefore, all the received messages are new and have to be relayed to the respective client handlers
}

/// Finds the response to the open message of the client that started the poller.
/// Returns all the messages following this message (inclusive).
pub fn filter_messages_of_first_polling_iteration<'a>(
    messages: &'a mut Vec<CanisterOutputMessage>,
    first_client_key: ClientKey,
) {
    // the filter assumes that, if the response to the open message of the first client that connects after the gateway reboots is not (yet) present,
    // the polled messages (which are all old) do not contain the result to a previous open message that the same client sent before the gateway rebooted

    // if this is not the case, the filter will return all the messages from the result of the open message of the client which reconnects first
    // however, this open message is old as it was sent by the client before the gateway rebooted and this is mistaken with
    // the result of the new open message has not yet been pushed to the message queue of the canister (and thus not polled)

    // this assumption is valid as the client is identified by its principal and a nonce which is generated for each new IC WS connection by the SDk
    // therefore, if the same client reconnects, the client key will be different and the scenario mentioned above does not happen
    let len_before_filter = messages.len();
    messages.reverse();
    let mut keep = true;
    messages.retain(|canister_output_message| {
        if keep {
            let websocket_message: WebsocketMessage = from_slice(&canister_output_message.content)
                .expect("content of canister_output_message is not of type WebsocketMessage");
            if websocket_message.is_service_message {
                let canister_service_message = decode_one(&websocket_message.content)
                    .expect("content of websocket_message is not of type CanisterServiceMessage");
                if let CanisterServiceMessage::OpenMessage(CanisterOpenMessageContent {
                    client_key,
                }) = canister_service_message
                {
                    if client_key == first_client_key {
                        keep = false;
                    }
                }
            }
            return true;
        }
        false
    });
    // in case the response to the open message to the first client that connects is not found, all messages have to be discarded
    // as they correspond to messages sent before the gateway rebooted
    if keep == true {
        messages.retain(|_m| false);
    }
    messages.reverse();
    trace!(
        "Filtered out {} polled messages",
        len_before_filter - messages.len()
    );
}

async fn signal_termination_and_cleanup(
    agent: &Arc<Agent>,
    poller_to_main_channel: &mut Sender<TerminationInfo>,
    canister_id: Principal,
    messages_demux: Arc<RwLock<MessagesDemux>>,
    e: String,
) {
    // let the main task know that this poller will terminate due to a CDK error
    signal_poller_task_termination(
        poller_to_main_channel,
        TerminationInfo::CdkError(canister_id),
    )
    .await;
    // let each client connection handler task connected to this poller know that the poller will terminate
    // and thus they also have to close the WebSocket connection and terminate
    for (client_key, (client_channel_tx, _parent_span)) in
        messages_demux.read().await.client_channels()
    {
        call_ws_close_in_background(
            agent.clone(),
            canister_id,
            client_key.to_owned(),
            Span::current(),
        );
        if let Err(channel_err) = client_channel_tx
            .send(IcWsConnectionUpdate::Error(format!(
                "Terminating poller task due to error: {}",
                e
            )))
            .await
        {
            error!("Client's thread terminated: {}", channel_err);
        }
    }
}

async fn signal_poller_task_termination(
    channel: &mut Sender<TerminationInfo>,
    info: TerminationInfo,
) {
    if let Err(e) = channel.send(info).await {
        error!(
            "Receiver has been dropped on the pollers connection manager's side. Error: {:?}",
            e
        );
    }
}

async fn sleep(millis: u64) {
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

fn call_ws_close_in_background(
    agent: Arc<Agent>,
    canister_id: Principal,
    client_key: ClientKey,
    span: Span,
) {
    // close client connection on canister
    // sending the request to the canister takes a few seconds
    // therefore this is done in a separate task
    // in order to not slow down the poller task
    tokio::spawn(
        async move {
            debug!("Calling ws_close for client");
            // TODO: figure out why it takes 10-30 seconds for the canister to close the connection
            if let Err(e) = canister_methods::ws_close(
                &agent,
                &canister_id,
                CanisterWsCloseArguments { client_key },
            )
            .await
            {
                error!("Calling ws_close on canister failed: {}", e);
            } else {
                debug!("Canister closed connection with client");
            }
            CLIENTS_REGISTERED_IN_CDK.fetch_sub(1, Ordering::SeqCst);
        }
        .instrument(span),
    );
}

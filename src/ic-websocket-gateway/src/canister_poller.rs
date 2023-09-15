use crate::{
    canister_methods::{
        self, CanisterOutputCertifiedMessages, CanisterOutputMessage, CanisterServiceMessage,
        CanisterWsGetMessagesArguments, CanisterWsStatusArguments, CanisterWsStatusResult,
        ClientPrincipal, WebsocketMessage,
    },
    events_analyzer::{Events, EventsCollectionType, EventsReference},
    metrics::canister_poller_metrics::{
        IncomingCanisterMessageEvents, IncomingCanisterMessageEventsMetrics, PollerEvents,
        PollerEventsMetrics,
    },
};
use candid::{CandidType, Decode};
use ic_agent::{export::Principal, Agent};
use rand::distributions::{Distribution, Uniform};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
};
use tracing::{debug, error, info, warn};

type CanisterGetMessagesWithEvents = (CanisterOutputCertifiedMessages, PollerEvents);

#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct CanisterToClientMessage {
    pub key: String,
    #[serde(with = "serde_bytes")]
    pub content: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub cert: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub tree: Vec<u8>,
}

/// ends of the channels needed by each canister poller tasks:
/// - main_to_poller: receiving side of the channel used by the main task to send the receiving task of a new client's channel to the poller task
/// - poller_to_main: sending side of the channel used by the poller to send the canister id of the poller which is about to terminate
#[derive(Debug)]
pub struct PollerChannelsPollerEnds {
    main_to_poller: Receiver<PollerToClientChannelData>,
    poller_to_main: Sender<TerminationInfo>,
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

pub enum IcWsConnectionUpdate {
    Message(CanisterToClientMessage),
    Error(String),
}

/// contains the information that the main sends to the poller task:
/// - NewClientChannel: sending side of the channel use by the poller to send messages to the client
/// - ClientDisconnected: signals the poller which cllient disconnected
#[derive(Debug, Clone)]
pub enum PollerToClientChannelData {
    NewClientChannel(u64, ClientPrincipal, Sender<IcWsConnectionUpdate>),
    ClientDisconnected(ClientPrincipal),
}

/// determines the reason of the poller task termination:
/// - LastClientDisconnected: last client disconnected and therefore there is no need to continue polling
/// - CdkError: error while polling the canister
pub enum TerminationInfo {
    LastClientDisconnected(Principal),
    CdkError(Principal),
}

pub struct CanisterPoller {
    canister_id: Principal,
    agent: Arc<Agent>,
    polling_interval_ms: u64,
    send_status_interval_ms: u64,
}

impl CanisterPoller {
    pub fn new(
        canister_id: Principal,
        agent: Arc<Agent>,
        polling_interval_ms: u64,
        send_status_interval_ms: u64,
    ) -> Self {
        Self {
            canister_id,
            agent,
            polling_interval_ms,
            send_status_interval_ms,
        }
    }

    #[tracing::instrument(
        name = "poll_canister",
        skip_all,
        fields(
            canister_id = %self.canister_id
        )
    )]
    pub async fn run_polling(&self, mut poller_channels: PollerChannelsPollerEnds) {
        // once the poller starts running, it requests messages from nonce 0.
        // if the canister already has some messages in the queue and receives the nonce 0, it knows that the poller restarted
        // therefore, it sends the last X messages to the gateway. From these, the gateway has to determine the response corresponding to the client's ws_open request
        let mut message_nonce = 0;

        // channels used to communicate with client's WebSocket task
        let mut client_channels: HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>> =
            HashMap::new();

        let mut clients_message_queues: HashMap<ClientPrincipal, Vec<CanisterToClientMessage>> =
            HashMap::new();

        let mut polling_iteration = 0; // used as a reference for the PollerEvents
        let get_messages_operation = self.get_canister_updates(message_nonce, polling_iteration);
        // pin the tracking of the in-flight asynchronous operation so that in each select! iteration get_messages_operation is continued
        // instead of issuing a new call to get_canister_updates
        tokio::pin!(get_messages_operation);

        // status index used by the CDK to detect when the WS Gateway fails
        let mut gateway_status_index: u64 = 0;
        let gateway_status_operation = self.send_status_message_to_canister(gateway_status_index);
        tokio::pin!(gateway_status_operation);

        'poller_loop: loop {
            select! {
                // receive channel used to send canister updates to new client's task
                Some(channel_data) = poller_channels.main_to_poller.recv() => {
                    match channel_data {
                        PollerToClientChannelData::NewClientChannel(_client_id, client_principal, client_channel) => {
                            debug!("Added new channel to poller for client: {:?}", client_principal);
                            client_channels.insert(client_principal.clone(), client_channel);
                        },
                        PollerToClientChannelData::ClientDisconnected(client_principal) => {
                            debug!("Removed client channel from poller for client {:?}", client_principal);
                            client_channels.remove(&client_principal);
                            debug!("Removed message queue from poller for client {:?}", client_principal);
                            clients_message_queues.remove(&client_principal);
                            debug!("{} clients connected to poller", client_channels.len());
                            // exit task if last client disconnected
                            if client_channels.is_empty() {
                                info!("Terminating poller task as no clients are connected");
                                signal_poller_task_termination(&mut poller_channels.poller_to_main, TerminationInfo::LastClientDisconnected(self.canister_id)).await;
                                break;
                            }
                        }
                    }
                }
                // poll canister for updates across multiple select! iterations
                res = &mut get_messages_operation => {
                    // process messages in queues before the ones just polled from the canister (if any) so that the clients receive messages in the expected order
                    clients_message_queues = process_queues(clients_message_queues, &client_channels).await;

                    if let Some((msgs, mut poller_events)) = res {
                        poller_events.metrics.set_start_relaying_messages();
                        poller_channels.poller_to_analyzer.send(Box::new(poller_events)).await.expect("analyzer's side of the channel dropped");
                        if message_nonce != 0 {
                            for canister_output_message in msgs.messages {
                                if let Err(cdk_err) = self.relay_message(canister_output_message, msgs.cert.clone(), msgs.tree.clone(), &client_channels, &poller_channels, &mut clients_message_queues, &mut message_nonce).await {
                                    error!("Terminating poller task due to CDK error: {}", cdk_err);
                                    signal_termination_and_cleanup(
                                        &mut poller_channels.poller_to_main,
                                        self.canister_id,
                                        &client_channels,
                                        cdk_err,
                                    )
                                    .await;
                                    break 'poller_loop;
                                }
                            }
                        }
                        else {
                            // if the poller just started (message_nonce == 0), the canister might have already had other messages in the queue which we should not send to the clients
                            // therefore, starting from the last message polled, we check the first message of type CanisterServiceMessage::OpenMessage, send it to the respective client
                            // and set message_nonce to the respective nonce so that in the next iteration the poller polls from there on

                            // keep track of the principals of all the clients in the poller's state so that we can send the open message to each of them
                            let mut principals: BTreeSet<&Principal> = client_channels.iter().map(|(principal, _)| principal).collect();
                            let mut messages_to_be_sent = Vec::new();
                            for canister_output_message in msgs.messages.iter().rev() {
                                if principals.contains(&&canister_output_message.client_principal) {
                                    let websocket_message = Decode!(&canister_output_message.content, WebsocketMessage).expect("content of canister_output_message is not of type WebsocketMessage");
                                    if websocket_message.is_service_message {
                                        let canister_service_message = Decode!(&websocket_message.content, CanisterServiceMessage).expect("content of websocket_message is not of type CanisterServiceMessage");
                                        if let CanisterServiceMessage::OpenMessage(_) = canister_service_message {
                                            // push messages to be sent to clients to a queue
                                            // these messages are pushed in reverse order compared to how they are received from the canister
                                            messages_to_be_sent.push(canister_output_message);
                                            // once the poller relays the open message for a connected client, the corresponding principal can be removed from the temporary set of principals
                                            principals.remove(&canister_output_message.client_principal);
                                            // once the set of principals is empty, we can stop processing the remaining canister messages as these were sent before the poller started
                                            if principals.len() == 0 {
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            // reverse the order of the messages in the queue so that they are in the same order as when received from the canister
                            // this way the message_nonce is set to the one of the open message of the last client that connected
                            for canister_output_message in messages_to_be_sent.iter().rev() {
                                if let Err(cdk_err) = self.relay_message(canister_output_message.to_owned().clone(), msgs.cert.clone(), msgs.tree.clone(), &client_channels, &poller_channels, &mut clients_message_queues, &mut message_nonce).await {
                                    error!("Terminating poller task due to CDK error: {}", cdk_err);
                                    signal_termination_and_cleanup(
                                        &mut poller_channels.poller_to_main,
                                        self.canister_id,
                                        &client_channels,
                                        cdk_err,
                                    )
                                    .await;
                                    break 'poller_loop;
                                }
                            }

                        }
                        // counting only iterations which return at least one canister message
                        polling_iteration += 1;
                    }

                    // pin a new asynchronous operation so that it can be restarted in the next select! iteration and continued in the following ones
                    get_messages_operation.set(self.get_canister_updates(message_nonce, polling_iteration));
                },
                res = &mut gateway_status_operation => {
                    match res {
                        Ok(_) => {
                            gateway_status_index += 1;
                            gateway_status_operation.set(
                                self.send_status_message_to_canister(gateway_status_index)
                            );
                        }
                        Err(e) => {
                            error!("Terminating poller task due to fail in sending gateway status update: {}", e);
                            signal_termination_and_cleanup(&mut poller_channels.poller_to_main, self.canister_id, &client_channels, e).await;
                            break 'poller_loop;
                        }
                    }
                }
            }
        }
    }

    async fn get_canister_updates(
        &self,
        message_nonce: u64,
        polling_iteration: u64,
    ) -> Option<CanisterGetMessagesWithEvents> {
        let mut poller_events = PollerEvents::new(
            Some(EventsReference::Iteration(polling_iteration)),
            EventsCollectionType::PollerStatus,
            PollerEventsMetrics::default(),
        );
        poller_events.metrics.set_start_polling();
        sleep(self.polling_interval_ms).await;
        // get messages to be relayed to clients from canister (starting from 'message_nonce')
        let canister_result = canister_methods::ws_get_messages(
            &self.agent,
            &self.canister_id,
            CanisterWsGetMessagesArguments {
                nonce: message_nonce,
            },
        )
        .await
        .ok()?;
        poller_events.metrics.set_received_messages();
        if canister_result.messages.len() > 0 {
            return Some((canister_result, poller_events));
        }
        None
    }

    async fn send_status_message_to_canister(&self, status_index: u64) -> CanisterWsStatusResult {
        let message = CanisterWsStatusArguments { status_index };

        // this logic is performed here, instead of in the branch corresponding to the current future, in order to not slow down the select! loop while waiting for the retries
        // (as once a future is selected, the corresponding branch "blocks" all the futures until the next iteration of select!)
        if let Err(e) =
            ws_status_to_canister_with_retries(Arc::clone(&self.agent), self.canister_id, message)
                .await
        {
            // in case of error, we report it immediately
            return Err(e);
        }
        info!("Sent gateway status update: {}", status_index);
        // if the ws_message is successful, we wait the interval for the gateway status check before returning
        sleep(self.send_status_interval_ms).await;
        Ok(())
    }

    async fn relay_message(
        &self,
        canister_output_message: CanisterOutputMessage,
        cert: Vec<u8>,
        tree: Vec<u8>,
        client_channels: &HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>>,
        poller_channels: &PollerChannelsPollerEnds,
        clients_message_queues: &mut HashMap<ClientPrincipal, Vec<CanisterToClientMessage>>,
        message_nonce: &mut u64,
    ) -> Result<(), String> {
        let mut incoming_canister_message_events = IncomingCanisterMessageEvents::new(
            None,
            EventsCollectionType::CanisterMessage,
            IncomingCanisterMessageEventsMetrics::default(),
        );
        incoming_canister_message_events
            .metrics
            .set_start_relaying_message();
        let client_principal = canister_output_message.client_principal;
        let m = CanisterToClientMessage {
            key: canister_output_message.key.clone(),
            content: canister_output_message.content,
            cert,
            tree,
        };

        let last_message_nonce = get_nonce_from_message(&canister_output_message.key)?;
        incoming_canister_message_events.reference =
            Some(EventsReference::MessageNonce(last_message_nonce));
        match client_channels.get(&client_principal) {
            Some(client_channel_tx) => {
                debug!("Received message with key: {:?} from canister", m.key);
                if let Err(e) = client_channel_tx
                    .send(IcWsConnectionUpdate::Message(m))
                    .await
                {
                    error!("Client's thread terminated: {}", e);
                    incoming_canister_message_events
                        .metrics
                        .set_no_message_relayed();
                } else {
                    incoming_canister_message_events
                        .metrics
                        .set_message_relayed();
                }
                poller_channels
                    .poller_to_analyzer
                    .send(Box::new(incoming_canister_message_events))
                    .await
                    .expect("analyzer's side of the channel dropped");
            },
            None => {
                // TODO: we should distinguish the case in which there is no client channel because the client's state hasn't been registered (yet)
                //       from the case in which the client has just disconnected (and its client channel removed before the polling returns new messages fot that client)
                warn!("Connection to client with principal: {:?} not opened yet. Adding message with key: {:?} to queue", m.key, client_principal);
                if let Some(message_queue) = clients_message_queues.get_mut(&client_principal) {
                    message_queue.push(m);
                } else {
                    clients_message_queues.insert(client_principal, vec![m]);
                }
            },
        }
        *message_nonce = last_message_nonce + 1;
        Ok(())
    }
}

async fn ws_status_to_canister_with_retries(
    agent: Arc<Agent>,
    canister_id: Principal,
    message: CanisterWsStatusArguments,
) -> Result<(), String> {
    let max_retries = 3;
    for attempt in 1..=max_retries {
        match canister_methods::ws_status(&agent, &canister_id, message.clone()).await {
            Ok(_) => {
                break;
            },
            Err(e) => {
                warn!(
                    "Attempt {}: called ws_message on canister failed: {}",
                    attempt, e
                );
                if attempt == max_retries {
                    return Err(String::from("Max retries for calling ws_message reached"));
                }
                let retry_delay_ms = sample_uniform(2, 5) as u64 * 1000;
                warn!("Retrying in {} milliseconds...", retry_delay_ms);
                sleep(retry_delay_ms).await;
            },
        }
    }
    Ok(())
}

async fn process_queues(
    clients_message_queues: HashMap<ClientPrincipal, Vec<CanisterToClientMessage>>,
    client_channels: &HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>>,
) -> HashMap<ClientPrincipal, Vec<CanisterToClientMessage>> {
    // TODO: make sure that messages are delivered to each client in the order defined by their sequence numbers

    let mut to_be_stored = HashMap::new();

    for (client_principal, message_queue) in clients_message_queues {
        match client_channels.get(&client_principal) {
            Some(client_channel_tx) => {
                // once a client channel is received, messages for that client will not be put in the queue anymore (until that client disconnects)
                // thus the respective queue does not need to be stored
                for m in message_queue {
                    warn!("Processing message with key: {:?} from queue", m.key);
                    if let Err(e) = client_channel_tx
                        .send(IcWsConnectionUpdate::Message(m))
                        .await
                    {
                        error!("Client's thread terminated: {}", e);
                    }
                }
            },
            None => {
                // if the client channel has not been received yet, keep the messages in the queue
                to_be_stored.insert(client_principal, message_queue);
            },
        };
    }

    to_be_stored
}

fn sample_uniform(lower_bound: i32, upper_bound: i32) -> i32 {
    let mut rng = rand::thread_rng();
    let uniform = Uniform::new_inclusive(lower_bound, upper_bound);
    uniform.sample(&mut rng)
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

async fn signal_termination_and_cleanup(
    poller_to_main_channel: &mut Sender<TerminationInfo>,
    canister_id: Principal,
    client_channels: &HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>>,
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
    for client_channel_tx in client_channels.values() {
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

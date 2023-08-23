use crate::{
    canister_methods::{
        self, CanisterIncomingMessage, CanisterOutputCertifiedMessages, CanisterWsMessageResult,
        ClientPublicKey, GatewayStatusMessage,
    },
    events_analyzer::{Events, EventsCollectionType, EventsReference},
    metrics::canister_poller_metrics::{
        IcWsEstablishmentNotificationEvents, IcWsEstablishmentNotificationEventsMetrics,
        IncomingCanisterMessageEvents, IncomingCanisterMessageEventsMetrics, PollerEvents,
        PollerEventsMetrics,
    },
};
use candid::CandidType;
use ic_agent::{export::Principal, Agent};
use rand::distributions::{Distribution, Uniform};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
};
use tracing::{debug, error, info, warn};

type CanisterGetMessagesWithEvents = (CanisterOutputCertifiedMessages, PollerEvents);

#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct CertifiedMessage {
    pub key: String,
    #[serde(with = "serde_bytes")]
    pub val: Vec<u8>,
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
    Message(CertifiedMessage),
    Established,
    Error(String),
}

/// contains the information that the main sends to the poller task:
/// - NewClientChannel: sending side of the channel use by the poller to send messages to the client
/// - ClientDisconnected: signals the poller which cllient disconnected
#[derive(Debug, Clone)]
pub enum PollerToClientChannelData {
    NewClientChannel(u64, ClientPublicKey, Sender<IcWsConnectionUpdate>),
    ClientDisconnected(ClientPublicKey),
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
    pub async fn run_polling(
        &self,
        mut poller_channels: PollerChannelsPollerEnds,
        mut message_nonce: u64,
    ) {
        info!(
            "Created new poller task starting from nonce: {}",
            message_nonce
        );

        // channels used to communicate with client's WebSocket task
        let mut client_channels: HashMap<ClientPublicKey, Sender<IcWsConnectionUpdate>> =
            HashMap::new();

        let mut polling_iteration = 0;
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
                        PollerToClientChannelData::NewClientChannel(client_id, client_key, client_channel) => {
                            let agent = Arc::clone(&self.agent);
                            let canister_id = self.canister_id;
                            let client_channel_cl = client_channel.clone();
                            let client_key_cl = client_key.clone();
                            let poller_to_analyzer_channel = poller_channels.poller_to_analyzer.clone();
                            tokio::task::spawn(async move {
                                let mut ic_ws_establishment_notification_events = IcWsEstablishmentNotificationEvents::new(Some(EventsReference::ClientId(client_id)), EventsCollectionType::NewClientConnection, IcWsEstablishmentNotificationEventsMetrics::default());
                                ic_ws_establishment_notification_events.metrics.set_received_client_channel();

                                // notify client handler that the IC WS connection establishment is completed
                                if let Err(e) = client_channel_cl.send(IcWsConnectionUpdate::Established).await {
                                    error!("Client's thread terminated: {}", e);
                                }
                                ic_ws_establishment_notification_events.metrics.set_sent_client_notification();

                                // notify canister that it can now send messages for the client corresponding to client_key
                                let gateway_message =
                                    CanisterIncomingMessage::IcWebSocketEstablished(client_key_cl);
                                if let Err(err) = ws_message_to_canister_with_retries(agent, canister_id, gateway_message).await {
                                    error!("Failed to notify canister of client connection establishment: {:?}", err);
                                    // no need to terminate poller as the canister might have intentionally prevented this client from connecting
                                    // as we have already sent the 'IcWsConnectionUpdate::Established' message to the client we now have to tell it that the connection failed
                                    // we cannot first notify the canister and then the client hanlder as the canister might send a message immediately after receiving the notification (and the client hanlder would miss it)
                                    if let Err(e) = client_channel_cl.send(IcWsConnectionUpdate::Error(err)).await {
                                        error!("Client's thread terminated: {}", e);
                                    }
                                }
                                ic_ws_establishment_notification_events.metrics.set_sent_canister_notification();
                                poller_to_analyzer_channel
                                    .send(Box::new(ic_ws_establishment_notification_events))
                                    .await
                                    .expect("analyzer's side of the channel dropped");
                            });
                            // TODO: should be done only after establishment succeeds
                            debug!("Added new channel to poller for client: {:?}", client_key);
                            client_channels.insert(client_key.clone(), client_channel);
                        },
                        PollerToClientChannelData::ClientDisconnected(client_key) => {
                            debug!("Removed client channel from poller for client {:?}", client_key);
                            client_channels.remove(&client_key);
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
                    if let Some((msgs, mut poller_events)) = res {
                        poller_events.metrics.set_start_relaying_messages();
                        poller_channels.poller_to_analyzer.send(Box::new(poller_events)).await.expect("analyzer's side of the channel dropped");
                        for encoded_message in msgs.messages {
                            let mut incoming_canister_message_events = IncomingCanisterMessageEvents::new(None, EventsCollectionType::CanisterMessage, IncomingCanisterMessageEventsMetrics::default());
                            incoming_canister_message_events.metrics.set_start_relaying_message();
                            let client_key = encoded_message.client_key;
                            let m = CertifiedMessage {
                                key: encoded_message.key.clone(),
                                val: encoded_message.val,
                                cert: msgs.cert.clone(),
                                tree: msgs.tree.clone(),
                            };

                            match get_nonce_from_message(&encoded_message.key) {
                                Ok(last_message_nonce) => {
                                    incoming_canister_message_events.reference = Some(EventsReference::MessageNonce(last_message_nonce));
                                    match client_channels.get(&client_key) {
                                        Some(client_channel_tx) => {
                                            debug!("Received message with key: {:?} from canister", m.key);
                                            if let Err(e) = client_channel_tx.send(IcWsConnectionUpdate::Message(m)).await {
                                                error!("Client's thread terminated: {}", e);
                                                incoming_canister_message_events.metrics.set_no_message_relayed();
                                            }
                                            else {
                                                incoming_canister_message_events.metrics.set_message_relayed();
                                            }
                                            poller_channels.poller_to_analyzer.send(Box::new(incoming_canister_message_events)).await.expect("analyzer's side of the channel dropped");
                                        },
                                        None => error!("Connection to client with key: {:?} closed before message could be delivered", client_key)
                                    }
                                    message_nonce = last_message_nonce + 1;
                                },
                                Err(cdk_err) => {
                                    error!("Terminating poller task due to CDK error: {}", cdk_err);
                                    signal_termination_and_cleanup(&mut poller_channels.poller_to_main, self.canister_id, &client_channels, cdk_err).await;
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
        let canister_result =
            canister_methods::ws_get_messages(&self.agent, &self.canister_id, message_nonce)
                .await
                .ok()?;
        poller_events.metrics.set_received_messages();
        if canister_result.messages.len() > 0 {
            return Some((canister_result, poller_events));
        }
        None
    }

    async fn send_status_message_to_canister(&self, status_index: u64) -> CanisterWsMessageResult {
        let message = CanisterIncomingMessage::IcWebSocketGatewayStatus(GatewayStatusMessage {
            status_index,
        });

        // this logic is performed here, instead of in the branch corresponding to the current future, in order to not slow down the select! loop while waiting for the retries
        // (as once a future is selected, the corresponding branch "blocks" all the futures until the next iteration of select!)
        if let Err(e) =
            ws_message_to_canister_with_retries(Arc::clone(&self.agent), self.canister_id, message)
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
}

async fn ws_message_to_canister_with_retries(
    agent: Arc<Agent>,
    canister_id: Principal,
    message: CanisterIncomingMessage,
) -> Result<(), String> {
    let max_retries = 3;
    for attempt in 1..=max_retries {
        match canister_methods::ws_message(&agent, &canister_id, message.clone()).await {
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
    client_channels: &HashMap<ClientPublicKey, Sender<IcWsConnectionUpdate>>,
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

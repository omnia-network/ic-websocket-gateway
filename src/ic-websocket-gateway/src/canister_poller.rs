use candid::CandidType;
use ic_agent::{export::Principal, Agent};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
};

use crate::canister_methods::{
    self, CanisterIncomingMessage, CanisterOutputCertifiedMessages, ClientPublicKey,
    GatewayStatusMessage,
};

const GATEWAY_STATUS_INTERVAL: u64 = 30_000;

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
}

impl PollerChannelsPollerEnds {
    pub fn new(
        main_to_poller: Receiver<PollerToClientChannelData>,
        poller_to_main: Sender<TerminationInfo>,
    ) -> Self {
        Self {
            main_to_poller,
            poller_to_main,
        }
    }
}

/// contains the information that the main sends to the poller task:
/// - NewClientChannel: sending side of the channel use by the poller to send messages to the client
/// - ClientDisconnected: signals the poller which cllient disconnected
#[derive(Debug, Clone)]
pub enum PollerToClientChannelData {
    NewClientChannel(ClientPublicKey, Sender<Result<CertifiedMessage, String>>),
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
}

impl CanisterPoller {
    pub fn new(canister_id: Principal, agent: Arc<Agent>) -> Self {
        Self { canister_id, agent }
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
        polling_interval: u64,
    ) {
        info!(
            "Created new poller task starting from nonce: {}",
            message_nonce
        );

        // channels used to communicate with client's WebSocket task
        let mut client_channels: HashMap<
            ClientPublicKey,
            Sender<Result<CertifiedMessage, String>>,
        > = HashMap::new();

        let get_messages_operation = get_canister_updates(
            &self.agent,
            self.canister_id,
            message_nonce,
            polling_interval,
        );
        // pin the tracking of the in-flight asynchronous operation so that in each select! iteration get_messages_operation is continued
        // instead of issuing a new call to get_canister_updates
        tokio::pin!(get_messages_operation);

        // status index used by the CDK to detect when the WS Gateway fails
        static IC_WS_GATEWAY_STATUS_INDEX: AtomicU64 = AtomicU64::new(0);
        let gateway_status_operation = sleep(GATEWAY_STATUS_INTERVAL);
        tokio::pin!(gateway_status_operation);

        'poller_loop: loop {
            select! {
                // receive channel used to send canister updates to new client's task
                Some(channel_data) = poller_channels.main_to_poller.recv() => {
                    match channel_data {
                        PollerToClientChannelData::NewClientChannel(client_key, client_channel) => {
                            info!("Added new channel to poller for client: {:?}", client_key);
                            client_channels.insert(client_key, client_channel);
                        },
                        PollerToClientChannelData::ClientDisconnected(client_key) => {
                            info!("Removed client channel from poller for client {:?}", client_key);
                            client_channels.remove(&client_key);
                            info!("{} clients connected to poller", client_channels.len());
                            // exit task if last client disconnected
                            if client_channels.is_empty() {
                                signal_poller_task_termination(&mut poller_channels.poller_to_main, TerminationInfo::LastClientDisconnected(self.canister_id)).await;
                                info!("Terminating poller task as no clients are connected");
                                break;
                            }
                        }
                    }
                }
                // poll canister for updates across multiple select! iterations
                Ok(msgs) = &mut get_messages_operation => {
                    for encoded_message in msgs.messages {
                        let client_key = encoded_message.client_key;
                        let m = CertifiedMessage {
                            key: encoded_message.key.clone(),
                            val: encoded_message.val,
                            cert: msgs.cert.clone(),
                            tree: msgs.tree.clone(),
                        };

                        match client_channels.get(&client_key) {
                            Some(client_channel_tx) => {
                                info!("Received message with key: {:?} from canister", m.key);
                                if let Err(e) = client_channel_tx.send(Ok(m)).await {
                                    error!("Client's thread terminated: {}", e);
                                }
                            },
                            None => error!("Connection to client with key: {:?} closed before message could be delivered", client_key)
                        }

                        match get_nonce_from_message(encoded_message.key) {
                            Ok(last_message_nonce) => message_nonce = last_message_nonce + 1,
                            Err(cdk_err) => {
                                // let the main task know that this poller will terminate due to a CDK error
                                signal_poller_task_termination(&mut poller_channels.poller_to_main, TerminationInfo::CdkError(self.canister_id)).await;
                                // let each client connection handler task connected to this poller know that the poller will terminate
                                // and thus they also have to close the WebSocket connection and terminate
                                for client_channel_tx in client_channels.values() {
                                    if let Err(channel_err) = client_channel_tx.send(Err(format!("Terminating poller task due to CDK error: {}", cdk_err))).await {
                                        error!("Client's thread terminated: {}", channel_err);
                                    }
                                }
                                error!("Terminating poller task due to CDK error: {}", cdk_err);
                                break 'poller_loop;
                            }
                        }
                    }

                    // pin a new asynchronous operation so that it can be restarted in the next select! iteration and continued in the following ones
                    get_messages_operation.set(get_canister_updates(&self.agent, self.canister_id, message_nonce, polling_interval));
                },
                _ = &mut gateway_status_operation => {
                    let agent = self.agent.clone();
                    let canister_id = self.canister_id;
                    // notify the CDK about the status of the WS Gateway
                    tokio::spawn(async move {
                        let ic_ws_status_index = IC_WS_GATEWAY_STATUS_INDEX.load(Ordering::SeqCst);
                        let gateway_message = CanisterIncomingMessage::IcWebSocketGatewayStatus(GatewayStatusMessage {
                            status_index: ic_ws_status_index,
                        });
                        if let Err(e) = canister_methods::ws_message(&agent, &canister_id, gateway_message).await {
                            error!("Calling ws_message on canister failed: {}", e);
                            // TODO: try again or report failure
                        }
                        else {
                            info!("Sent gateway status update: {}", ic_ws_status_index);
                            IC_WS_GATEWAY_STATUS_INDEX.fetch_add(1, Ordering::SeqCst);
                        }
                    });
                    gateway_status_operation.set(sleep(GATEWAY_STATUS_INTERVAL));
                }
            }
        }
    }
}

async fn get_canister_updates(
    agent: &Agent,
    canister_id: Principal,
    message_nonce: u64,
    polling_interval: u64,
) -> Result<CanisterOutputCertifiedMessages, String> {
    sleep(polling_interval).await;
    // get messages to be relayed to clients from canister (starting from 'message_nonce')
    canister_methods::ws_get_messages(agent, &canister_id, message_nonce).await
}

fn get_nonce_from_message(key: String) -> Result<u64, String> {
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

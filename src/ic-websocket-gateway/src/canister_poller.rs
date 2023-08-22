use candid::CandidType;
use ic_agent::{export::Principal, Agent};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
};

use crate::canister_methods::{
    self, CanisterIncomingMessage, CanisterWsGetMessagesArguments, CanisterWsGetMessagesResult,
    CanisterWsMessageArguments, CanisterWsMessageResult, ClientPublicKey, GatewayStatusMessage,
};

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
    NewClientChannel(
        ClientPublicKey,
        Sender<Result<CanisterToClientMessage, String>>,
    ),
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
        let mut client_channels: HashMap<
            ClientPublicKey,
            Sender<Result<CanisterToClientMessage, String>>,
        > = HashMap::new();

        let get_messages_operation = self.get_canister_updates(message_nonce);
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
                        PollerToClientChannelData::NewClientChannel(client_key, client_channel) => {
                            debug!("Added new channel to poller for client: {:?}", client_key);
                            client_channels.insert(client_key, client_channel);
                        },
                        PollerToClientChannelData::ClientDisconnected(client_key) => {
                            debug!("Removed client channel from poller for client {:?}", client_key);
                            client_channels.remove(&client_key);
                            debug!("{} clients connected to poller", client_channels.len());
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
                // TODO: in the current implementation, if this call fails,
                //       a new call is not pinned again. We have to handle the error case
                Ok(msgs) = &mut get_messages_operation => {
                    for canister_output_messages in msgs.messages {
                        let client_key = canister_output_messages.client_key;
                        let m = CanisterToClientMessage {
                            key: canister_output_messages.key.clone(),
                            content: canister_output_messages.content,
                            cert: msgs.cert.clone(),
                            tree: msgs.tree.clone(),
                        };

                        match client_channels.get(&client_key) {
                            Some(client_channel_tx) => {
                                debug!("Received message with key: {:?} from canister", m.key);
                                if let Err(e) = client_channel_tx.send(Ok(m)).await {
                                    error!("Client's thread terminated: {}", e);
                                }
                            },
                            None => error!("Connection to client with key: {:?} closed before message could be delivered", client_key)
                        }

                        match get_nonce_from_message(canister_output_messages.key) {
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
                    get_messages_operation.set(self.get_canister_updates(message_nonce));
                },
                _ = &mut gateway_status_operation => {
                    gateway_status_index += 1;
                    gateway_status_operation.set(
                        self.send_status_message_to_canister(gateway_status_index)
                    );
                }
            }
        }
    }

    async fn get_canister_updates(&self, message_nonce: u64) -> CanisterWsGetMessagesResult {
        sleep(self.polling_interval_ms).await;
        // get messages to be relayed to clients from canister (starting from 'message_nonce')
        canister_methods::ws_get_messages(
            &self.agent,
            &self.canister_id,
            CanisterWsGetMessagesArguments {
                nonce: message_nonce,
            },
        )
        .await
    }

    async fn send_status_message_to_canister(&self, status_index: u64) -> CanisterWsMessageResult {
        let message = CanisterIncomingMessage::IcWebSocketGatewayStatus(GatewayStatusMessage {
            status_index,
        });

        let res = canister_methods::ws_message(
            &self.agent,
            &self.canister_id,
            CanisterWsMessageArguments { msg: message },
        )
        .await;

        match res.clone() {
            Ok(_) => info!("Sent gateway status update: {}", status_index),
            Err(e) => {
                error!("Calling ws_message on canister failed: {}", e);
                // TODO: try again or report failure
            },
        }

        sleep(self.send_status_interval_ms).await;

        res
    }
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

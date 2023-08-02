use candid::CandidType;
use ic_agent::{export::Principal, Agent};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
};

use crate::canister_methods::{self, CanisterOutputCertifiedMessages, ClientPublicKey};

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
    poller_to_main: Sender<Principal>,
}

impl PollerChannelsPollerEnds {
    pub fn new(
        main_to_poller: Receiver<PollerToClientChannelData>,
        poller_to_main: Sender<Principal>,
    ) -> Self {
        Self {
            main_to_poller,
            poller_to_main,
        }
    }
}

/// contains the information that the main sends to the poller task
/// - NewClientChannel: sending side of the channel use by the poller to send messages to the client
/// - ClientDisconnected: signals the poller which cllient disconnected
#[derive(Debug, Clone)]
pub enum PollerToClientChannelData {
    NewClientChannel(ClientPublicKey, Sender<CertifiedMessage>),
    ClientDisconnected(ClientPublicKey),
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
        mut nonce: u64,
        polling_interval: u64,
    ) {
        info!("Created new poller task starting from nonce: {}", nonce);

        // channels used to communicate with client's WebSocket task
        let mut client_channels: HashMap<ClientPublicKey, Sender<CertifiedMessage>> =
            HashMap::new();

        let get_messages_operation =
            get_canister_updates(&self.agent, self.canister_id, nonce, polling_interval);
        // pin the tracking of the in-flight asynchronous operation so that in each select! iteration get_messages_operation is continued
        // instead of issuing a new call to get_canister_updates
        tokio::pin!(get_messages_operation);

        loop {
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
                                signal_poller_task_termination(&mut poller_channels.poller_to_main, self.canister_id).await;
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
                            Some(client_channel_rx) => {
                                info!("Received message with key: {:?} from canister", m.key);
                                if let Err(e) = client_channel_rx.send(m).await {
                                    error!("Client's thread terminated: {}", e);
                                }
                            },
                            None => error!("Connection to client with key: {:?} closed before message could be delivered", client_key)
                        }

                        match get_nonce_from_message(encoded_message.key) {
                            Ok(last_nonce) => nonce = last_nonce + 1,
                            Err(_e) => {
                                panic!("TODO: graceful shutdown of poller task and related clients disconnection");
                            }
                        }
                    }

                    // pin a new asynchronous operation so that it can be restarted in the next select! iteration and continued in the following ones
                    get_messages_operation.set(get_canister_updates(&self.agent, self.canister_id, nonce, polling_interval));
                }
            }
        }
    }
}

async fn get_canister_updates(
    agent: &Agent,
    canister_id: Principal,
    nonce: u64,
    polling_interval: u64,
) -> Result<CanisterOutputCertifiedMessages, String> {
    tokio::time::sleep(Duration::from_millis(polling_interval)).await;
    canister_methods::ws_get_messages(agent, &canister_id, nonce).await
}

fn get_nonce_from_message(key: String) -> Result<u64, String> {
    if let Some(nonce_str) = key.split('_').last() {
        let nonce = nonce_str
            .parse()
            .map_err(|e| format!("Could not parse nonce. Error: {:?}", e))?;
        return Ok(nonce);
    }
    Err(String::from(
        "Key in canister message is not formatted correctly",
    ))
}

async fn signal_poller_task_termination(channel: &mut Sender<Principal>, canister_id: Principal) {
    if let Err(e) = channel.send(canister_id).await {
        error!(
            "Receiver has been dropped on the pollers connection manager's side. Error: {:?}",
            e
        );
    }
}

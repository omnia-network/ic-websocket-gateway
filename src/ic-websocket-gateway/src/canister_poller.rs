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

    pub async fn run_polling(
        &self,
        mut poller_chnanels: PollerChannelsPollerEnds,
        mut nonce: u64,
        polling_interval: u64,
    ) {
        // channels used to communicate with client's WebSocket task
        let mut client_channels: HashMap<ClientPublicKey, Sender<CertifiedMessage>> =
            HashMap::new();
        info!("Poller started from nonce: {}", nonce);
        loop {
            select! {
                // TODO: prevent client connection updates from starving get_canister_updates !!!

                // receive channel used to send canister updates to new client's task
                Some(channel_data) = poller_chnanels.main_to_poller.recv() => {
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
                                poller_chnanels.poller_to_main.send(self.canister_id).await.expect("channel with main should be open");
                                info!("Terminating poller task");
                                break;
                            }
                        }
                    }
                }
                // poll canister for updates
                msgs = get_canister_updates(&self.agent, self.canister_id, nonce, polling_interval) => {
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
                                info!("Sending message with key: {:?} to client handler task", m.key);
                                if let Err(e) = client_channel_rx.send(m).await {
                                    error!("Client's thread terminated: {}", e);
                                }
                            },
                            None => error!("Connection with client with key {:?} closed before message could be delivered", client_key)
                        }

                        nonce = encoded_message
                            .key
                            .split('_')
                            .last()
                            .unwrap()
                            .parse()
                            .unwrap();
                        nonce += 1
                    }
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
) -> CanisterOutputCertifiedMessages {
    // !!! the polling interval implies the maximum frequency of the client connections !!!
    // if clients connect at a higher freqency than 1/polling_interval, the messages from poller to clients will be starved
    // by the clients connecting to each poller
    tokio::time::sleep(Duration::from_millis(polling_interval)).await;

    canister_methods::ws_get_messages(agent, &canister_id, nonce)
        .await
        .unwrap()
}

use candid::CandidType;
use ic_agent::{export::Principal, Agent};
use serde::{Deserialize, Serialize};

use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    select,
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
};

use crate::canister_methods::{self, CanisterOutputCertifiedMessages, ClientPublicKey};

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
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
    main_to_poller: UnboundedReceiver<PollerToClientChannelData>,
    poller_to_main: UnboundedSender<Principal>,
}

impl PollerChannelsPollerEnds {
    pub fn new(
        main_to_poller: UnboundedReceiver<PollerToClientChannelData>,
        poller_to_main: UnboundedSender<Principal>,
    ) -> Self {
        Self {
            main_to_poller,
            poller_to_main,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PollerToClientChannelData {
    NewClientChannel(ClientPublicKey, UnboundedSender<CertifiedMessage>),
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

    pub fn get_canister_id(&self) -> Principal {
        self.canister_id.clone()
    }

    pub async fn run_polling(
        &self,
        mut poller_chnanels: PollerChannelsPollerEnds,
        mut nonce: u64,
        polling_interval: u64,
    ) {
        // channels used to communicate with client's WebSocket task
        let mut client_channels: HashMap<ClientPublicKey, UnboundedSender<CertifiedMessage>> =
            HashMap::new();
        println!(
            "Started poller: canister: {}, nonce: {}",
            self.canister_id, nonce
        );
        loop {
            select! {
                // receive channel used to send canister updates to new client's task
                Some(channel_data) = poller_chnanels.main_to_poller.recv() => {
                    match channel_data {
                        PollerToClientChannelData::NewClientChannel(client_key, client_channel) => {
                            println!("Adding new client poller channel: canister: {}, client {:?}", self.canister_id, client_key);
                            client_channels.insert(client_key, client_channel);
                        },
                        PollerToClientChannelData::ClientDisconnected(client_key) => {
                            println!("Removing client poller channel: canister: {}, client {:?}", self.canister_id, client_key);
                            client_channels.remove(&client_key);
                            // exit task if last client disconnected
                            if client_channels.is_empty() {
                                println!("Last client disconnected, terminating poller task: canister {}", self.canister_id);
                                poller_chnanels.poller_to_main.send(self.canister_id).expect("channel with main should be open");
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
                                if let Err(e) = client_channel_rx.send(m) {
                                    println!("Client's thread terminated: {}", e);
                                }
                            },
                            None => println!("Connection with client closed before message could be delivered")
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
    tokio::time::sleep(Duration::from_millis(polling_interval)).await;

    canister_methods::ws_get_messages(agent, &canister_id, nonce)
        .await
        .unwrap()
}

use candid::Principal;
use ic_agent::Agent;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tracing::warn;

use crate::canister_methods::{self, CanisterStatus, GetCanisterUpdatesArgs};

pub struct ManagementCanisterPoller {
    canister_id: Principal,
    /// nonce specified by the gateway during the query call to ws_get_messages, used by the CDK to determine which messages to send
    message_nonce: Arc<RwLock<u64>>,
    registered_canisters: BTreeSet<Principal>,
    agent: Arc<Agent>,
    polling_interval_ms: u64,
}

impl ManagementCanisterPoller {
    pub fn new(canister_id: Principal, agent: Arc<Agent>, polling_interval_ms: u64) -> Self {
        Self {
            canister_id,
            // once the poller starts running, it requests messages from nonce 0.
            // if the canister already has some messages in the queue and receives the nonce 0, it knows that the poller restarted
            // therefore, it sends the last X messages to the gateway. From these, the gateway has to determine the response corresponding to the client's ws_open request
            message_nonce: Arc::new(RwLock::new(0)),
            registered_canisters: BTreeSet::new(),
            agent,
            polling_interval_ms,
        }
    }

    pub async fn start_polling(&mut self) {
        self.registered_canisters = {
            canister_methods::register_gateway(&self.agent, &self.canister_id)
                .await
                .expect("error after calling ws_open")
                .iter()
                .fold(BTreeSet::new(), |mut s, principal| {
                    s.insert(principal.to_owned());
                    s
                })
        };
        warn!("Registered canisters: {:?}", self.registered_canisters);
        // TODO: start poller for each registered principal
        loop {
            sleep(self.polling_interval_ms).await;
            // get messages to be relayed to clients from canister (starting from 'message_nonce')
            let canister_updates = canister_methods::get_canister_updates(
                &self.agent,
                &self.canister_id,
                GetCanisterUpdatesArgs {
                    nonce: *self.message_nonce.read().await,
                },
            )
            .await
            .expect("error after calling get_canister_updates");

            let messages_count = canister_updates.len();
            for canister_update in canister_updates {
                match canister_update.status {
                    CanisterStatus::Registered(principal) => {
                        warn!("Registered new canister: {:?}", principal);
                        self.registered_canisters.insert(principal);
                        // TODO: start poller
                    },
                    CanisterStatus::Deregistered(principal) => {
                        warn!("Deregistered canister: {:?}", principal);
                        self.registered_canisters.remove(&principal);
                        // TODO: stop poller
                    },
                }
            }
            let last_iteration_nonce = *self.message_nonce.read().await;
            *self.message_nonce.write().await = last_iteration_nonce + messages_count as u64;
        }
    }
}

async fn sleep(millis: u64) {
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

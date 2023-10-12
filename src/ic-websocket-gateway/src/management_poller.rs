use candid::Principal;
use ic_agent::Agent;
use std::{sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tracing::warn;

use crate::canister_methods::{self, GetNewRegisteredCanistersArgs};

pub struct ManagementCanisterPoller {
    canister_id: Principal,
    /// nonce specified by the gateway during the query call to ws_get_messages, used by the CDK to determine which messages to send
    message_nonce: Arc<RwLock<u64>>,
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
            agent,
            polling_interval_ms,
        }
    }

    pub async fn start_polling(&self) {
        canister_methods::register_gateway(&self.agent, &self.canister_id)
            .await
            .expect("error after calling ws_open");
        loop {
            sleep(self.polling_interval_ms).await;
            // get messages to be relayed to clients from canister (starting from 'message_nonce')
            let canister_result = canister_methods::get_new_registered_canisters(
                &self.agent,
                &self.canister_id,
                GetNewRegisteredCanistersArgs {
                    nonce: *self.message_nonce.read().await,
                },
            )
            .await
            .expect("error after calling ws_get_messages");

            let messages_count = canister_result.len();
            warn!("Received {:?} messages", messages_count);
            for canister_registration in canister_result {
                // TODO: start a poller for each canister principal received
                warn!("Received: {:?}", canister_registration.content);
            }
            let last_iteration_nonce = *self.message_nonce.read().await;
            *self.message_nonce.write().await = last_iteration_nonce + messages_count as u64;
        }
    }
}

async fn sleep(millis: u64) {
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

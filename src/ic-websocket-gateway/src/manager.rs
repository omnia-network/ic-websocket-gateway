use dashmap::{mapref::entry::Entry, DashMap};
use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use std::sync::Arc;
use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};
use tracing::{info, warn, Span};

use crate::{
    canister_methods::{self, ClientKey},
    canister_poller::IcWsCanisterUpdate,
    events_analyzer::Events,
    ws_listener::{TlsConfig, WsListener},
};

/// State of the WS Gateway that can be shared between threads
pub type GatewaySharedState = Arc<GatewayState>;

/// State of the WS Gateway consisting of the principal of each canister being polled
/// and the status of each poller
pub struct GatewayState(DashMap<CanisterPrincipal, PollerStatus>);

impl GatewayState {
    pub fn insert_client_channel_and_get_new_poller_state(
        &self,
        canister_id: CanisterPrincipal,
        client_key: ClientKey,
        client_channel_tx: Sender<IcWsCanisterUpdate>,
        client_session_span: Span,
    ) -> Result<Option<PollerState>, String> {
        // TODO: figure out if this is actually atomic
        match self.0.entry(canister_id) {
            Entry::Occupied(mut entry) => {
                // the poller has already been started
                // if the poller is active, add client key and sender end of the channel to the poller state
                if let PollerStatus::Active(poller_state) = entry.get_mut() {
                    poller_state.insert(
                        client_key,
                        ClientSender {
                            sender: client_channel_tx.clone(),
                            span: client_session_span,
                        },
                    );
                    // the poller shall not be started again
                    return Ok(None);
                }
                // if the poller is 'Failed', the client session shall not be started
                Err(String::from("Poller is in 'Failed' status"))
            },
            Entry::Vacant(entry) => {
                // the poller has not been started yet
                // initialize the poller state and add client key and sender end of the channel
                let poller_state = Arc::new(DashMap::with_capacity_and_shard_amount(1024, 1024));
                poller_state.insert(
                    client_key,
                    ClientSender {
                        sender: client_channel_tx.clone(),
                        span: client_session_span,
                    },
                );
                entry.insert(PollerStatus::Active(Arc::clone(&poller_state)));
                // the poller shall be started
                Ok(Some(Arc::clone(&poller_state)))
            },
        }
    }

    pub fn remove_client(&self, canister_id: CanisterPrincipal, client_key: ClientKey) {
        // TODO: figure out if this is actually atomic
        if let Entry::Occupied(mut entry) = self.0.entry(canister_id) {
            if let PollerStatus::Active(poller_state) = entry.get_mut() {
                if poller_state.remove(&client_key).is_none() {
                    // as the client was connected, the poller state must contain an entry for 'client_key'
                    // if this is encountered it might indicate a race condition
                    unreachable!("Client key not found in poller state");
                }
                // even if this is the last client session for the canister, do not remove the canister from the gateway state
                // this will be done by the poller task
            }
            // if the poller status is 'Failed', the client will be removed upon receiving the error update from the canister
        } else {
            // the gateway state must contain an entry for 'canister_id' of the canister which the client was connected to
            // if this is encountered it might indicate a race condition
            unreachable!("Canister not found in gateway state");
        }
    }

    pub fn remove_client_if_exists(
        &self,
        canister_id: CanisterPrincipal,
        client_key: ClientKey,
    ) -> bool {
        // TODO: figure out if this is actually atomic
        if let Entry::Occupied(mut entry) = self.0.entry(canister_id) {
            if let PollerStatus::Active(poller_state) | PollerStatus::Failed(poller_state) =
                entry.get_mut()
            {
                // even if this is the last client session for the canister, do not remove the canister from the gateway state
                // this will be done by the poller task
                // returns true if the client was removed, false if there was no such client
                return poller_state.remove(&client_key).is_some();
            }
            unreachable!("poller status must be 'Active' or 'Failed'");
        }
        // this can happen when clients try to connect when the poller status is 'Failed'
        // trying to setup the connection will return an error and therefore the client will not be inserted in the poller state
        // therefore, there is no need to remove it
        false
    }

    pub fn remove_canister_if_empty(&self, canister_id: CanisterPrincipal) -> bool {
        // SAFETY:
        // remove_if returns None if the condition is not met, otherwise it returns the Some(<entry>)
        // if None is returned, the poller state is not empty and therefore there are still clients connected and the poller shall not terminate
        // if Some is returned, the poller state is empty and therefore the poller shall terminate
        self.0
            .remove_if(&canister_id, |_, poller_status| {
                if let PollerStatus::Active(poller_state) = poller_status {
                    return poller_state.is_empty();
                }
                // if the poller status is 'Failed', the poller shall terminate
                true
            })
            .is_some()
    }

    pub fn set_poller_status_to_failed(&self, canister_id: CanisterPrincipal) {
        self.0.alter(&canister_id, |_canister_id, poller_status| {
            if let PollerStatus::Active(poller_state) = poller_status {
                return PollerStatus::Failed(poller_state);
            }
            unreachable!("cannot be called on 'Failed' poller status");
        })
    }
}

/// Status of the poller containing the poller state, if Active
pub enum PollerStatus {
    Active(PollerState),
    Failed(PollerState),
}

/// State of each poller consisting of the keys of the clients connected to the poller
/// and the state associated to each client
pub type PollerState = Arc<DashMap<ClientKey, ClientSender>>;

/// State of each client consisting of the sender side of the channel used to send canister updates to the client
/// and the span associated to the client session
#[derive(Debug)]
pub struct ClientSender {
    pub sender: Sender<IcWsCanisterUpdate>,
    pub span: ClientSessionSpan,
}

pub type CanisterPrincipal = Principal;

pub type ClientSessionSpan = Span;

/// Manager of the WS Gateway maintaining its state
pub struct Manager {
    /// Agent used to interact with the IC
    agent: Arc<Agent>,
    /// Gateway  address
    address: String,
    /// Sender side of the channel used to send events from different components to the events analyzer
    analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
    /// State of the WS Gateway
    state: GatewaySharedState,
}

impl Manager {
    pub async fn new(
        gateway_address: String,
        ic_network_url: String,
        identity: BasicIdentity,
        analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
    ) -> Self {
        let agent = canister_methods::get_new_agent(&ic_network_url, identity)
            .await
            .expect("could not get new agent");
        let agent = Arc::new(agent);
        info!(
            "Gateway Agent principal: {}",
            agent.get_principal().expect("Principal should be set")
        );

        // creates a concurrent hashmap with capacity of 32 divided in shards so that each entry can be accessed concurrently without locking the whole state
        let state: GatewaySharedState = Arc::new(GatewayState(
            DashMap::with_capacity_and_shard_amount(32, 32),
        ));

        return Self {
            agent,
            address: gateway_address,
            analyzer_channel_tx,
            state,
        };
    }

    /// Keeps accepting incoming connections
    pub fn start_accepting_incoming_connections(
        &self,
        tls_config: Option<TlsConfig>,
        rate_limiting_channel_rx: Receiver<Option<f64>>,
        polling_interval: u64,
    ) -> JoinHandle<()> {
        // spawn a task which keeps listening for incoming client connections
        let gateway_address = self.address.clone();
        let agent = Arc::clone(&self.agent);
        let gateway_shared_state = Arc::clone(&self.state);
        let analyzer_channel_tx = self.analyzer_channel_tx.clone();
        tokio::spawn(async move {
            let mut ws_listener = WsListener::new(
                &gateway_address,
                agent,
                gateway_shared_state,
                analyzer_channel_tx,
                rate_limiting_channel_rx,
                polling_interval,
                tls_config,
            )
            .await;

            info!("Start accepting incoming connections");
            ws_listener.listen_for_incoming_requests().await;
            info!("Stopped accepting incoming connections");
        })
    }
}

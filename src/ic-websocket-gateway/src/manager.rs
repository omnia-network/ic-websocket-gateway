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
/// and the state associated to each poller
pub struct GatewayState(DashMap<CanisterPrincipal, PollerState>);

impl GatewayState {
    pub fn insert_client_channel_and_get_new_poller_state(
        &self,
        canister_id: CanisterPrincipal,
        client_key: ClientKey,
        client_channel_tx: Sender<IcWsCanisterUpdate>,
    ) -> Option<PollerState> {
        // TODO: figure out if this is actually atomic
        match self.0.entry(canister_id) {
            Entry::Occupied(mut entry) => {
                // the poller has already been started
                // add client key and sender end of the channel to the poller state
                let poller_state = entry.get_mut();
                poller_state.insert(
                    client_key,
                    ClientSender {
                        sender: client_channel_tx.clone(),
                        span: Span::current(),
                    },
                );
                None
            },
            Entry::Vacant(entry) => {
                // the poller has not been started yet
                // initialize the poller state and add client key and sender end of the channel
                let poller_state = Arc::new(DashMap::with_capacity_and_shard_amount(1024, 1024));
                poller_state.insert(
                    client_key,
                    ClientSender {
                        sender: client_channel_tx.clone(),
                        span: Span::current(),
                    },
                );
                entry.insert(Arc::clone(&poller_state));
                Some(Arc::clone(&poller_state))
            },
        }
    }

    pub fn remove_client(&self, canister_id: CanisterPrincipal, client_key: ClientKey) {
        // TODO: figure out if this is actually atomic
        if let Entry::Occupied(mut entry) = self.0.entry(canister_id) {
            let poller_state = entry.get_mut();
            if poller_state.remove(&client_key).is_none() {
                // as the client was connected, the poller state must contain an entry for 'client_key'
                // if this is encountered it might indicate a race condition
                unreachable!("Client key not found in poller state");
            }
            // even if this is the last client session for the canister, do not remove the canister from the gateway state
            // this will be done by the poller task
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
            let poller_state = entry.get_mut();
            // even if this is the last client session for the canister, do not remove the canister from the gateway state
            // this will be done by the poller task
            poller_state.remove(&client_key).is_none()
        } else {
            // the gateway state must contain an entry for 'canister_id' of the canister which the client was connected to
            // if this is encountered it might indicate a race condition
            unreachable!("Canister not found in gateway state");
        }
    }

    pub fn remove_canister_if_empty(&self, canister_id: CanisterPrincipal) -> bool {
        // SAFETY:
        // remove_if returns None if the condition is not met, otherwise it returns the Some(<entry>)
        // if None is returned, the poller state is not empty and therefore there are still clients connected and the poller should not terminate
        // if Some is returned, the poller state is empty and therefore the poller should terminate
        self.0
            .remove_if(&canister_id, |_, poller_state| poller_state.is_empty())
            .is_some()
    }
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
        let fetch_ic_root_key = ic_network_url != "https://icp0.io";

        let agent = canister_methods::get_new_agent(&ic_network_url, identity, fetch_ic_root_key)
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

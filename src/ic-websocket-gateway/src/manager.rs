use dashmap::{mapref::entry::Entry, DashMap};
use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use std::sync::Arc;
use tokio::{sync::mpsc::Sender, task::JoinHandle};
use tracing::{info, Span};

use crate::{
    canister_methods::{self, ClientKey},
    canister_poller::IcWsCanisterMessage,
    ws_listener::{TlsConfig, WsListener},
};

/// State of the WS Gateway that can be shared between threads
pub type GatewaySharedState = Arc<GatewayState>;

/// State of the WS Gateway consisting of the principal of each canister being polled
/// and the state of each poller
pub struct GatewayState(DashMap<CanisterPrincipal, PollerState>);

impl GatewayState {
    pub fn new() -> Self {
        Self(DashMap::with_capacity_and_shard_amount(32, 32))
    }
}

impl GatewayState {
    pub fn insert_client_channel_and_get_new_poller_state(
        &self,
        canister_id: CanisterPrincipal,
        client_key: ClientKey,
        client_channel_tx: Sender<IcWsCanisterMessage>,
        client_session_span: Span,
    ) -> Option<PollerState> {
        // TODO: figure out if this is actually atomic
        match self.0.entry(canister_id) {
            Entry::Occupied(mut entry) => {
                // the poller has already been started
                // if the poller is active, add client key and sender end of the channel to the poller state
                let poller_state = entry.get_mut();
                poller_state.insert(
                    client_key,
                    ClientSender {
                        sender: client_channel_tx.clone(),
                        span: client_session_span,
                    },
                );
                // the poller shall not be started again
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
                        span: client_session_span,
                    },
                );
                entry.insert(Arc::clone(&poller_state));
                // the poller shall be started
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
        }
        // this can happen when the poller has failed and the poller state has already been removed
        // indeed, a client session might enter the Close state before the poller side of the channel has been dropped - but after the poller state has been removed -
        // in such a case, the client state as already been removed by the poller, together with the whole poller state
        // therefore there is no need to do anything else here
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
            // returns true if the client was removed, false if there was no such client
            return poller_state.remove(&client_key).is_some();
        }
        // this can happen when the poller has failed and the poller state has already been removed
        // indeed, a client session might get an error before the poller side of the channel has been dropped - but after the poller state has been removed -
        // in such a case, the client state as already been removed by the poller, together with the whole poller state
        // therefore there is no need to do anything else here
        false
    }

    pub fn remove_canister_if_empty(&self, canister_id: CanisterPrincipal) -> bool {
        // SAFETY:
        // remove_if returns None if the condition is not met, otherwise it returns the Some(<entry>)
        // if None is returned, the poller state is not empty and therefore there are still clients connected and the poller shall not terminate
        // if Some is returned, the poller state is empty and therefore the poller shall terminate
        self.0
            .remove_if(&canister_id, |_, poller_state| poller_state.is_empty())
            .is_some()
    }

    pub fn remove_failed_canister(&self, canister_id: CanisterPrincipal) {
        if let None = self.0.remove(&canister_id) {
            unreachable!("failed canister not found in gateway state");
        }
    }
}

/// State of each poller consisting of the keys of the clients connected to the poller
/// and the state associated to each client
pub type PollerState = Arc<DashMap<ClientKey, ClientSender>>;

/// State of each client consisting of the sender side of the channel used to send canister updates to the client
/// and the span associated to the client session
#[derive(Debug)]
pub struct ClientSender {
    pub sender: Sender<IcWsCanisterMessage>,
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
    /// State of the WS Gateway
    state: GatewaySharedState,
}

impl Manager {
    pub async fn new(
        gateway_address: String,
        ic_network_url: String,
        identity: BasicIdentity,
    ) -> Self {
        let agent = canister_methods::get_new_agent(&ic_network_url, identity)
            .await
            .expect("could not get new agent");
        let agent = Arc::new(agent);

        // creates a concurrent hashmap with capacity of 32 divided in shards so that each entry can be accessed concurrently without locking the whole state
        let state: GatewaySharedState = Arc::new(GatewayState::new());

        return Self {
            agent,
            address: gateway_address,
            state,
        };
    }

    pub fn get_agent_principal(&self) -> Principal {
        self.agent.get_principal().expect("Principal should be set")
    }

    /// Keeps accepting incoming connections
    pub fn start_accepting_incoming_connections(
        &self,
        tls_config: Option<TlsConfig>,
        polling_interval: u64,
    ) -> JoinHandle<()> {
        // spawn a task which keeps listening for incoming client connections
        let gateway_address = self.address.clone();
        let agent = Arc::clone(&self.agent);
        let gateway_shared_state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let mut ws_listener = WsListener::new(
                &gateway_address,
                agent,
                gateway_shared_state,
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

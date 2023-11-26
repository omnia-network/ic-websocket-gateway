use dashmap::DashMap;
use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use std::sync::Arc;
use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};
use tracing::{info, Span};

use crate::{
    canister_methods::{self, ClientKey},
    canister_poller::IcWsCanisterUpdate,
    events_analyzer::Events,
    ws_listener::{TlsConfig, WsListener},
};

/// State of the WS Gateway consisting of the principal of each canister being pollered
/// and the state associated to each poller
pub type GatewayState = Arc<DashMap<CanisterPrincipal, PollerState>>;

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
    state: GatewayState,
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
        let state: GatewayState = Arc::new(DashMap::with_capacity_and_shard_amount(32, 32));

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
        let gateway_state = Arc::clone(&self.state);
        let analyzer_channel_tx = self.analyzer_channel_tx.clone();
        tokio::spawn(async move {
            let mut ws_listener = WsListener::new(
                &gateway_address,
                agent,
                gateway_state,
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

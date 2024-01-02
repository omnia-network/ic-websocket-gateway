use crate::ws_listener::{TlsConfig, WsListener};
use canister_utils::get_new_agent;
use concurrent_map::{GatewaySharedState, GatewayState};
use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::info;

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
        let agent = get_new_agent(&ic_network_url, identity)
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

use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    select, signal,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, span, warn, Level, Span};

use crate::{
    canister_methods::{self, ClientKey},
    canister_poller::{
        CanisterPoller, IcWsConnectionUpdate, PollerChannelsPollerEnds, PollerToClientChannelData,
        TerminationInfo,
    },
    client_connection_handler::IcWsConnectionState,
    events_analyzer::{Events, EventsCollectionType, EventsReference},
    metrics::gateway_server_metrics::{
        ConnectionEstablishmentEvents, ConnectionEstablishmentEventsMetrics,
    },
    ws_listener::{TlsConfig, WsListener},
};

/// contains the information needed by the WS Gateway to maintain the state of the WebSocket connection
#[cfg(not(test))] // only compile and run the following block when not running tests
#[derive(Debug, Clone)]
pub struct ClientSession {
    client_id: u64,
    client_key: ClientKey,
    canister_id: Principal,
    message_for_client_tx: Sender<IcWsConnectionUpdate>,
    span: Span,
}

/// contains the information needed by the WS Gateway to maintain the state of the WebSocket connection
// set properties as public only for tests
#[cfg(test)] // only compile and run the following block when running tests
#[derive(Debug, Clone)]
pub struct ClientSession {
    pub client_id: u64,
    pub client_key: ClientKey,
    pub canister_id: Principal,
    pub message_for_client_tx: Sender<IcWsConnectionUpdate>,
    pub span: Span,
}

impl ClientSession {
    pub fn new(
        client_id: u64,
        client_key: ClientKey,
        canister_id: Principal,
        message_for_client_tx: Sender<IcWsConnectionUpdate>,
        span: Span,
    ) -> Self {
        Self {
            client_id,
            client_key,
            canister_id,
            message_for_client_tx,
            span,
        }
    }
}

/// WS Gateway
pub struct GatewayServer {
    /// agent used to interact with the canisters
    agent: Arc<Agent>,
    /// gateway address
    address: String,
    /// sender side of the channel used by the client's connection handler task to communicate the connection state to the main task
    client_connection_handler_tx: Sender<IcWsConnectionState>,
    /// receiver side of the channel used by the main task to get the state of the client connection from the connection handler task
    client_connection_handler_rx: Receiver<IcWsConnectionState>,
    /// sender side of the channel used to send events from different components to the analyzer
    events_channel_tx: Sender<Box<dyn Events + Send>>,
    /// state of the WS Gateway
    state: GatewayState,
    /// cancellation token used to signal other tasks when it's time to shut down
    token: CancellationToken,
}

impl GatewayServer {
    pub async fn new(
        gateway_address: String,
        subnet_url: String,
        identity: BasicIdentity,
        events_channel_tx: Sender<Box<dyn Events + Send>>,
    ) -> Self {
        let fetch_ic_root_key = subnet_url != "https://icp0.io";

        let agent = canister_methods::get_new_agent(&subnet_url, identity, fetch_ic_root_key)
            .await
            .expect("could not get new agent");
        let agent = Arc::new(agent);
        info!(
            "Gateway Agent principal: {}",
            agent.get_principal().expect("Principal should be set")
        );

        // [main task]                         [client connection handler task]
        // client_connection_handler_rx <----- client_connection_handler_tx

        // channel used to send the state of the client connection
        // the client connection handler task sends the session information when the WebSocket connection is established and
        // the id the of the client when the connection is closed
        let (client_connection_handler_tx, client_connection_handler_rx) = mpsc::channel(100);

        let token = CancellationToken::new();

        return Self {
            agent,
            address: gateway_address,
            client_connection_handler_tx,
            client_connection_handler_rx,
            events_channel_tx,
            state: GatewayState::default(),
            token,
        };
    }

    pub fn start_accepting_incoming_connections(
        &self,
        tls_config: Option<TlsConfig>,
        rate_limiting_channel_rx: Receiver<Option<f64>>,
    ) {
        // spawn a task which keeps listening for incoming client connections
        let gateway_address = self.address.clone();
        let agent = Arc::clone(&self.agent);
        let client_connection_handler_tx = self.client_connection_handler_tx.clone();
        let events_channel_tx = self.events_channel_tx.clone();
        let token = self.token.clone();
        tokio::spawn(async move {
            let mut ws_listener = WsListener::new(
                &gateway_address,
                agent,
                client_connection_handler_tx,
                events_channel_tx,
                rate_limiting_channel_rx,
                tls_config,
            )
            .await;

            debug!("Start accepting incoming connections");
            ws_listener.listen_for_incoming_requests(token).await;
            info!("Stopped accepting incoming connections");
        });
    }

    pub async fn manage_state(&mut self, polling_interval: u64) {
        // [main task]                             [poller task]
        // poller_channel_for_completion_rx <----- poller_channel_for_completion_tx

        // channel used by the poller task to let the main task know that the last client disconnected
        // and so the WS Gateway can cleanup the poller task data from its state
        let (poller_channel_for_completion_tx, mut poller_channel_for_completion_rx): (
            Sender<TerminationInfo>,
            Receiver<TerminationInfo>,
        ) = mpsc::channel(100);

        loop {
            select! {
                // check if a client's connection state changed
                Some(connection_state) = self.recv_from_client_connection_handler() => {
                    self.state.manage_clients_connections(
                        connection_state,
                        poller_channel_for_completion_tx.clone(),
                        self.events_channel_tx.clone(),
                        polling_interval,
                        self.agent.clone()
                    ).await;

                }
                // check if a poller task has terminated
                Some(termination_info) = poller_channel_for_completion_rx.recv() => {
                    match termination_info {
                        TerminationInfo::LastClientDisconnected(canister_id) => self.state.remove_poller_data(&canister_id),
                        TerminationInfo::CdkError(canister_id) => self.handle_failed_poller(&canister_id).await,
                    }
                },
                // detect ctrl_c signal from the OS
                _ = signal::ctrl_c() => break,
            }
        }
        self.graceful_shutdown(poller_channel_for_completion_rx)
            .await;
    }

    #[tracing::instrument(name = "manage_pollers_state", skip(self)
        fields(
            canister_id = %canister_id
        )
    )]
    async fn handle_failed_poller(&mut self, canister_id: &Principal) {
        // the client connection handlers are terminated directly by the poller via the direct channel between them
        error!("Removed all client data for canister");
        self.state.remove_poller_data(canister_id);
    }

    #[tracing::instrument(name = "graceful_shutdown", skip_all)]
    async fn graceful_shutdown(
        &mut self,
        mut poller_channel_for_completion_rx: Receiver<TerminationInfo>,
    ) {
        info!("Starting graceful shutdown");
        self.token.cancel();
        loop {
            if let Ok(IcWsConnectionState::Closed((client_key, canister_id))) =
                self.client_connection_handler_rx.try_recv()
            {
                // remove client's channel from poller, if it exists and is not finished
                if let Some(poller_channel_for_client_channel_sender_tx) =
                    self.state.connected_canisters.get_mut(&canister_id)
                {
                    // try sending message to poller task
                    if poller_channel_for_client_channel_sender_tx
                        .send(PollerToClientChannelData::ClientDisconnected(client_key))
                        .await
                        .is_err()
                    {
                        // if poller task is finished, remove its data from WS Gateway state
                        warn!("Poller task closed but data is still in state");
                        self.state.remove_poller_data(&canister_id)
                    }
                }
            }
            if let Ok(TerminationInfo::LastClientDisconnected(canister_id)) =
                poller_channel_for_completion_rx.try_recv()
            {
                self.state.remove_poller_data(&canister_id);
            }
            if self.state.count_connected_pollers() == 0 {
                info!("All pollers data has been removed from the gateway state");
                break;
            }
        }
    }

    pub async fn recv_from_client_connection_handler(&mut self) -> Option<IcWsConnectionState> {
        self.client_connection_handler_rx.recv().await
    }
}

/// state of the WS Gateway containing:
/// - canisters it is polling
struct GatewayState {
    /// maps the principal of the canister to the sender side of the channel used to communicate with the corresponding poller task
    connected_canisters: HashMap<Principal, Sender<PollerToClientChannelData>>,
}

impl GatewayState {
    fn default() -> Self {
        Self {
            connected_canisters: HashMap::default(),
        }
    }

    async fn manage_clients_connections(
        &mut self,
        connection_state: IcWsConnectionState,
        poller_channel_for_completion_tx: Sender<TerminationInfo>,
        events_channel_tx: Sender<Box<dyn Events + Send>>,
        polling_interval: u64,
        agent: Arc<Agent>,
    ) {
        match connection_state {
            IcWsConnectionState::Setup(client_session) => {
                let new_client_connection_span = span!(
                    parent: &client_session.span, Level::DEBUG, "new_client_connection", canister_id = %client_session.canister_id
                );
                let mut connection_establishment_events = ConnectionEstablishmentEvents::new(
                    Some(EventsReference::ClientId(client_session.client_id)),
                    EventsCollectionType::NewClientConnection,
                    ConnectionEstablishmentEventsMetrics::default(),
                );
                connection_establishment_events
                    .metrics
                    .set_received_client_session();

                let client_key = client_session.client_key.clone();
                let canister_id = client_session.canister_id;

                // contains the sending side of the channel created by the client's connection handler which needs to be sent
                // to the canister poller in order for it to be able to send messages directly to the client task
                let guard = new_client_connection_span.enter();
                let poller_to_client_channel_data = PollerToClientChannelData::NewClientChannel(
                    client_key.clone(),
                    client_session.message_for_client_tx.clone(),
                    Span::current(),
                );
                drop(guard);
                // check if client is connecting to a canister that is not yet being polled
                // if so, create new poller task
                let needs_new_poller = match self.connected_canisters.get_mut(&canister_id) {
                    Some(poller_channel_for_client_channel_sender_tx) => {
                        // !!! having data of the poller task in the WS Gateway state does not imply that the poller is still running !!!
                        // the poller task might have finished and the canister id sent to the main task via the poller_channel_for_completion channel
                        // however the main task handling loop might handle an incoming connection for the same canister before handling the cleanup
                        // therefore, the canister poller task might have terminated even if the data is still in the WS Gateway state
                        // try to send channel data to poller
                        poller_channel_for_client_channel_sender_tx
                            .send(poller_to_client_channel_data.clone())
                            .await
                            .is_err()
                    },
                    None => true,
                };

                if needs_new_poller {
                    // [main task]                                        [poller task]
                    // poller_channel_for_client_channel_sender_tx -----> poller_channel_for_client_channel_sender_rx

                    // channel used to communicate with the poller task
                    // the channel is used to send to the poller the sender side of a new client's channel
                    // so that the poller can send canister messages directly to the client's task
                    let (
                        poller_channel_for_client_channel_sender_tx,
                        poller_channel_for_client_channel_sender_rx,
                    ) = mpsc::channel(100);

                    self.add_poller_data(
                        canister_id,
                        poller_channel_for_client_channel_sender_tx.clone(),
                    );

                    let poller_channels_poller_ends = PollerChannelsPollerEnds::new(
                        poller_channel_for_client_channel_sender_rx,
                        poller_channel_for_completion_tx,
                        events_channel_tx.clone(),
                    );
                    let agent = Arc::clone(&agent);
                    new_client_connection_span
                        .in_scope(|| debug!("Client connecting to a new canister"));

                    // spawn new canister poller task
                    tokio::spawn(async move {
                        let mut poller = CanisterPoller::new(canister_id, agent, polling_interval);
                        // the channel used to send updates to the first client is passed as an argument to the poller
                        // this way we can be sure that once the poller gets the first messages from the canister, there is already a client to send them to
                        poller
                            .run_polling(
                                poller_channels_poller_ends,
                                client_key,
                                client_session.message_for_client_tx.clone(),
                                new_client_connection_span,
                            )
                            .await;
                        // once the poller terminates, return the canister id so that the poller data can be removed from the WS gateway state
                        canister_id
                    });
                    connection_establishment_events
                        .metrics
                        .set_started_new_poller();
                } else {
                    new_client_connection_span
                        .in_scope(|| debug!("Client connecting to an already polled canister"));
                    drop(new_client_connection_span);
                }
                connection_establishment_events
                    .metrics
                    .set_sent_client_channel_to_poller();

                events_channel_tx
                    .send(Box::new(connection_establishment_events))
                    .await
                    .expect("analyzer's side of the channel dropped");
            },
            IcWsConnectionState::Closed((client_key, canister_id)) => {
                // remove client's channel from poller, if it exists and is not finished
                if let Some(poller_channel_for_client_channel_sender_tx) =
                    self.connected_canisters.get_mut(&canister_id)
                {
                    // try sending message to poller task
                    if poller_channel_for_client_channel_sender_tx
                        .send(PollerToClientChannelData::ClientDisconnected(client_key))
                        .await
                        .is_err()
                    {
                        // if poller task is finished, remove its data from WS Gateway state
                        warn!("Poller task closed but data is still in state");
                        self.remove_poller_data(&canister_id)
                    }
                }
            },
            _ => unreachable!("should not receive variants other than 'Setup' and 'Closed'"),
        }

        let _entered = span!(Level::INFO, "manage_clients_state").entered();
    }

    #[tracing::instrument(
        name = "manage_pollers_state",
        skip(self, poller_channel_for_client_channel_sender_tx),
        fields(
            canister_id = %canister_id
        )
    )]
    fn add_poller_data(
        &mut self,
        canister_id: Principal,
        poller_channel_for_client_channel_sender_tx: Sender<PollerToClientChannelData>,
    ) {
        // register new poller and the channel used to send client's channels to it
        self.connected_canisters
            .insert(canister_id, poller_channel_for_client_channel_sender_tx);
        info!("Created poller task data");
    }

    #[tracing::instrument(
        name = "manage_pollers_state",
        skip(self),
        fields(
            canister_id = %canister_id
        )
    )]
    fn remove_poller_data(&mut self, canister_id: &Principal) {
        // poller task has terminated, remove it from the map
        self.connected_canisters.remove(canister_id);
        // TODO: make sure that all the clients that were connected to the canister are also removed
        info!("Removed poller task data");
    }

    fn count_connected_pollers(&self) -> usize {
        self.connected_canisters.len()
    }
}

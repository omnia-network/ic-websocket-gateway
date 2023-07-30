use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    select,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};
use tracing::info;

use crate::{
    canister_methods::{self, CanisterIncomingMessage, ClientPublicKey},
    canister_poller::{
        CanisterPoller, CertifiedMessage, PollerChannelsPollerEnds, PollerToClientChannelData,
    },
    client_connection_handler::{WsConnectionState, WsConnectionsHandler},
};

/// contains the information needed by the WS Gateway to maintain the state of the WebSocket connection
#[cfg(not(test))] // only compile and run the following block when not running tests
#[derive(Debug, Clone)]
pub struct GatewaySession {
    client_id: u64,
    client_key: ClientPublicKey,
    canister_id: Principal,
    message_for_client_tx: UnboundedSender<CertifiedMessage>,
    nonce: u64,
}

/// contains the information needed by the WS Gateway to maintain the state of the WebSocket connection
// set properties as public only for tests
#[cfg(test)] // only compile and run the following block when not running tests
#[derive(Debug, Clone)]
pub struct GatewaySession {
    pub client_id: u64,
    pub client_key: ClientPublicKey,
    pub canister_id: Principal,
    pub message_for_client_tx: UnboundedSender<CertifiedMessage>,
    pub nonce: u64,
}

impl GatewaySession {
    pub fn new(
        client_id: u64,
        client_key: Vec<u8>,
        canister_id: Principal,
        message_for_client_tx: UnboundedSender<CertifiedMessage>,
        nonce: u64,
    ) -> Self {
        Self {
            client_id,
            client_key,
            canister_id,
            message_for_client_tx,
            nonce,
        }
    }
}

/// WS Gateway
pub struct GatewayServer {
    // agent used to interact with the canisters
    agent: Arc<Agent>,
    // gateway address:
    address: String,
    // sender side of the channel used by the client's connection handler task to communicate the connection state to the main task
    client_connection_handler_tx: UnboundedSender<WsConnectionState>,
    // receiver side of the channel used by the main task to get the state of the client connection from the connection handler task
    client_connection_handler_rx: UnboundedReceiver<WsConnectionState>,
    // state of the WS Gateway
    state: GatewayState,
}

impl GatewayServer {
    pub async fn new(gateway_address: &str, subnet_address: &str, identity: BasicIdentity) -> Self {
        let fetch_ic_root_key = subnet_address != "icp0.io";

        let agent = Arc::new(
            canister_methods::get_new_agent(subnet_address, identity, fetch_ic_root_key).await,
        );

        println!(
            "Gateway Agent principal: {}",
            agent.get_principal().expect("Principal should be set")
        );

        // [main task]                         [client connection handler task]
        // client_connection_handler_rx <----- client_connection_handler_tx

        // channel used to send the state of the client connection
        // the client connection handler task sends the session information when the WebSocket connection is established and
        // the id the of the client when the connection is closed
        let (client_connection_handler_tx, client_connection_handler_rx) =
            mpsc::unbounded_channel();

        Self {
            agent,
            address: String::from(gateway_address),
            client_connection_handler_tx,
            client_connection_handler_rx,
            state: GatewayState::default(),
        }
    }

    pub fn start_accepting_incoming_connections(&self) {
        // spawn a task which keeps listening for incoming client connections
        let gateway_address = self.address.clone();
        let agent = Arc::clone(&self.agent);
        let client_connection_handler_tx = self.client_connection_handler_tx.clone();
        info!("Start accepting incoming connections");
        tokio::spawn(async move {
            let mut ws_connections_hanlders =
                WsConnectionsHandler::new(&gateway_address, agent, client_connection_handler_tx)
                    .await;
            ws_connections_hanlders.listen_for_incoming_requests().await;
        });
    }

    pub async fn manage_state(&mut self, polling_interval: u64) {
        // [main task]                             [poller task]
        // poller_channel_for_completion_rx <----- poller_channel_for_completion_tx

        // channel used by the poller task to let the main task know that the last client disconnected
        // and so the WS Gateway can cleanup the poller task data from its state
        let (poller_channel_for_completion_tx, mut poller_channel_for_completion_rx): (
            UnboundedSender<Principal>,
            UnboundedReceiver<Principal>,
        ) = mpsc::unbounded_channel();

        loop {
            select! {
                // check if a client's connection state changed
                Some(connection_state) = self.recv_from_client_connection_handler() => {
                    // connection state can contain either:
                    // - the GatewaySession if the connection was successful
                    // - the client_id if the connection was closed before the client was registered
                    // - a connection error
                    self.state.manage_clients_connections(connection_state, poller_channel_for_completion_tx.clone(), polling_interval, &self.agent);
                }
                // check if a poller task has terminated
                Some(canister_id) = poller_channel_for_completion_rx.recv() => {
                    println!("Received cleanup command from poller task: canister {:?}", canister_id.to_string());
                    self.state.remove_poller_data(&canister_id);
                }
            }
        }
    }

    pub async fn recv_from_client_connection_handler(&mut self) -> Option<WsConnectionState> {
        self.client_connection_handler_rx.recv().await
    }
}

/// state of the WS Gateway containing:
/// - canisters it is polling
/// - sessions with the clients connected to it via WebSocket
/// - id of each client
struct GatewayState {
    /// maps the principal of the canister to the sender side of the channel used to communicate with the corresponding poller task
    connected_canisters: HashMap<Principal, UnboundedSender<PollerToClientChannelData>>,
    /// maps the client's public key to the state of the client's session
    client_session_map: HashMap<ClientPublicKey, GatewaySession>,
    /// maps the client id to its public key
    // needed because when a client disconnects, we only know its id but in order to clean the state of the client's session
    // we need to know the public key of the client
    client_key_map: HashMap<u64, ClientPublicKey>,
}

impl GatewayState {
    fn default() -> Self {
        Self {
            connected_canisters: HashMap::default(),
            client_session_map: HashMap::default(),
            client_key_map: HashMap::default(),
        }
    }

    fn manage_clients_connections(
        &mut self,
        connection_state: WsConnectionState,
        poller_channel_for_completion_tx: UnboundedSender<Principal>,
        polling_interval: u64,
        agent: &Arc<Agent>,
    ) {
        match connection_state {
            WsConnectionState::ConnectionEstablished(gateway_session) => {
                let client_key = gateway_session.client_key.clone();
                let canister_id = gateway_session.canister_id.clone();

                // add client's session state to the WS Gateway state
                self.add_client(gateway_session.clone());

                // contains the sending side of the channel created by the client's connection handler which needs to be sent
                // to the canister poller in order for it to be able to send messages directly to the client task
                let poller_to_client_channel_data = PollerToClientChannelData::NewClientChannel(
                    client_key.clone(),
                    gateway_session.message_for_client_tx.clone(),
                );
                // check if client is connecting to a canister that is not yet being polled
                // if so, create new poller task
                let needs_new_poller = match self.connected_canisters.get_mut(&canister_id) {
                    Some(connected_canister) => {
                        // !!! having data of the poller task in the WS Gateway state does not imply that the poller is still running !!!
                        // the poller task might have finished and the canister id sent to the main task via the poller_channel_for_completion channel
                        // however the main task handling loop might handle an incoming connection for the same canister before handling the cleanup
                        // therefore, the canister poller task might have terminated even if the data is still in the WS Gateway state
                        // try to send channel data to poller
                        connected_canister
                            .send(poller_to_client_channel_data.clone())
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
                    ) = mpsc::unbounded_channel();

                    // TODO: main task keeps track of the clients connected to each poller, terminates poller and
                    //       cleans up its state once the last client of a poller disconnects
                    //       advantage: cleaner code
                    //       disadvantage: main task has to do more work

                    // register new poller and the channel used to send client's channels to it
                    self.connected_canisters.insert(
                        canister_id.clone(),
                        poller_channel_for_client_channel_sender_tx.clone(),
                    );
                    let poller_channels_poller_ends = PollerChannelsPollerEnds::new(
                        poller_channel_for_client_channel_sender_rx,
                        poller_channel_for_completion_tx,
                    );
                    let agent = Arc::clone(agent);

                    // spawn new canister poller task
                    tokio::spawn(async move {
                        let poller = CanisterPoller::new(canister_id.clone(), agent);
                        println!("Created new poller: canister: {}", canister_id);
                        // if a new poller thread is started due to a client connection, the poller needs to know the nonce of the last polled message
                        // as an old poller thread (closed due to all clients disconnecting) might have already polled messages from the canister
                        // the new poller thread should not get those same messages again
                        poller
                            .run_polling(
                                poller_channels_poller_ends,
                                gateway_session.nonce,
                                polling_interval,
                            )
                            .await;
                        // once the poller terminates, return the canister id so that the poller data can be removed from the WS gateway state
                        canister_id
                    });

                    // send channel data to poller
                    poller_channel_for_client_channel_sender_tx
                        .send(poller_to_client_channel_data)
                        .expect("poller channel should be open");
                }

                // notify canister that it can now send messages for the client corresponding to client_key
                let agent = Arc::clone(agent);
                tokio::spawn(async move {
                    let gateway_message =
                        CanisterIncomingMessage::IcWebSocketEstablished(client_key);
                    if let Err(e) =
                        canister_methods::ws_message(&agent, &canister_id, gateway_message).await
                    {
                        println!("Calling ws_message on canister failed: {}", e);
                        // TODO: try again or report failure to client
                    }
                });
            },
            WsConnectionState::ConnectionClosed(client_id) => {
                // cleanup client's session from WS Gateway state
                self.remove_client(client_id, &agent)
            },
            WsConnectionState::ConnectionError(e) => {
                // TODO: make sure that cleaning up is not needed
                println!("Connection handler terminated with an error: {:?}", e);
            },
        }

        println!("{} clients registered", self.client_session_map.len());
    }

    fn add_client(&mut self, gateway_session: GatewaySession) {
        let client_key = gateway_session.client_key.clone();
        let client_id = gateway_session.client_id.clone();

        self.client_key_map.insert(client_id, client_key.clone());
        self.client_session_map.insert(client_key, gateway_session);
    }

    fn remove_client(&mut self, client_id: u64, agent: &Agent) {
        match self.client_key_map.remove(&client_id) {
            Some(client_key) => {
                let gateway_session = self
                    .client_session_map
                    .remove(&client_key.clone())
                    .expect("gateway session should be registered");

                let agent_cl = agent.clone();
                let canister_id_cl = gateway_session.canister_id.clone();
                let client_key_cl = client_key.clone();
                // close client connection on canister
                // sending the request to the canister takes a few seconds
                // therefore this is done in a separate task
                // in order to not slow down the main task
                tokio::spawn(async move {
                    if let Err(e) =
                        canister_methods::ws_close(&agent_cl, &canister_id_cl, client_key_cl).await
                    {
                        println!("Calling ws_close on canister failed: {}", e);
                    }
                });

                // remove client's channel from poller, if it exists and is not finished
                match self
                    .connected_canisters
                    .get_mut(&gateway_session.canister_id)
                {
                    Some(poller_channel_for_client_channel_sender_tx) => {
                        // try sending message to poller task
                        if poller_channel_for_client_channel_sender_tx
                            .send(PollerToClientChannelData::ClientDisconnected(client_key))
                            .is_err()
                        {
                            // if poller task is finished, remove its data from WS Gateway state
                            self.remove_poller_data(&gateway_session.canister_id)
                        }
                    },
                    None => (),
                }
            },
            None => {
                println!("Client closed connection before being registered");
            },
        }
    }

    fn remove_poller_data(&mut self, canister_id: &Principal) {
        println!(
            "Removed poller data from WS Gateway: canister {:?}",
            canister_id.to_string()
        );
        // poller task has terminated, remove it from the map
        self.connected_canisters.remove(canister_id);
    }
}

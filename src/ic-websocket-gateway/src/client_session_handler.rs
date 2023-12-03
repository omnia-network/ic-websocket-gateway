use crate::{
    canister_poller::{CanisterPoller, IcWsCanisterMessage},
    client_session::{ClientSession, IcWsError, IcWsSessionState},
    events_analyzer::Events,
    manager::{CanisterPrincipal, GatewaySharedState, PollerState},
    ws_listener::ClientId,
};
use futures_util::StreamExt;
use ic_agent::Agent;
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_tungstenite::accept_async;
use tracing::{debug, error, info, span, trace, warn, Instrument, Level, Span};

/// Handler of a client IC WS session
pub struct ClientSessionHandler {
    /// Identifier of the client connection
    id: ClientId,
    /// Agent used to interact with the IC
    agent: Arc<Agent>,
    /// State of the gateway
    gateway_shared_state: GatewaySharedState,
    /// Sender side of the channel used to send events from different components to the events analyzer
    analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
    /// Polling interval in milliseconds
    polling_interval_ms: u64,
}

impl ClientSessionHandler {
    pub fn new(
        id: ClientId,
        agent: Arc<Agent>,
        gateway_shared_state: GatewaySharedState,
        analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
        polling_interval_ms: u64,
    ) -> Self {
        Self {
            id,
            agent,
            gateway_shared_state,
            analyzer_channel_tx,
            polling_interval_ms,
        }
    }

    /// Upgrades to a WebSocket connection and handles the client session
    pub async fn start_session<S: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        stream: S,
    ) -> Result<(), String> {
        match accept_async(stream).await {
            Ok(ws_stream) => {
                debug!("Accepted WebSocket connection");

                let (ws_write, ws_read) = ws_stream.split();

                // [client connection handler task]        [poller task]
                // client_channel_rx                <----- client_channel_tx

                // channel used by the poller task to send canister updates from the poller to the client session handler task
                // which will then forward it to the client via the WebSocket connection
                let (client_channel_tx, client_channel_rx): (
                    Sender<IcWsCanisterMessage>,
                    Receiver<IcWsCanisterMessage>,
                ) = mpsc::channel(100);

                let client_session_span =
                    span!(parent: &Span::current(), Level::TRACE, "Client Session");

                let client_session = ClientSession::init(
                    self.id,
                    self.agent.get_principal().expect("Principal should be set"),
                    client_channel_rx,
                    ws_write,
                    ws_read,
                    Arc::clone(&self.agent),
                )
                .instrument(client_session_span.clone())
                .await
                .map_err(|e| format!("Client session error: {:?}", e))?;

                client_session_span.in_scope(|| {
                    debug!("Client session initialized");
                });

                self.handle_client_session(
                    client_session,
                    Some(client_channel_tx),
                    client_session_span,
                )
                .instrument(Span::current())
                .await?;
                Ok(())
            },
            Err(e) => {
                // no cleanup needed on the WS Gateway state as the client's session has never been created
                Err(format!("Refused WebSocket connection {:?}", e))
            },
        }
    }

    /// Handles the client session by reacting to the changes in the session state
    async fn handle_client_session<S: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        mut client_session: ClientSession<S>,
        // passed in an option so that it can be taken once after session Setup without cloning it
        // it is important not to clone it as otherwise the client session will not receive None in case of a poller error
        mut client_channel_tx: Option<Sender<IcWsCanisterMessage>>,
        client_session_span: Span,
    ) -> Result<(), String> {
        // keeps trying to update the client session state
        // if a new state is returned, execute the corresponding logic
        // if no new state is returned, try to update the state again
        loop {
            match client_session
                .try_update_state()
                .instrument(client_session_span.clone())
                .await
            {
                Ok(Some(IcWsSessionState::Init)) => {
                    // no update can bring the session back to Init
                    // no need to cleanup as the client session has not been created yet
                    unreachable!("Updating the client session state cannot result in Init");
                },
                Ok(Some(IcWsSessionState::Setup(ws_open_message))) => {
                    // SAFETY:
                    // first, update the gateway state
                    // only then, relay the message to the IC
                    // this is necessary to guarantee that once the poller retrieves the response to the WS open message,
                    // the poller sees (in the gateway state) the sending side of the channel needed to relay the response to the client session handler.
                    // the message cannot be relayed before doing so because if a poller is already running,
                    // it might poll a response for the connecting client before it gets the sending side of the channel

                    let new_poller_state = self
                        .gateway_shared_state
                        .insert_client_channel_and_get_new_poller_state(
                            client_session
                                .canister_id
                                .expect("must be set during Setup"),
                            client_session
                                .client_key
                                .clone()
                                .expect("must be set during Setup"),
                            // important not to clone 'client_channel_tx' as otherwise the client session will not receive None in case of a poller error
                            client_channel_tx.take().expect("must be set only once"),
                            client_session_span.clone(),
                        );

                    // TODO: figure out if it is guaranteed that all threads see the updated state of the gateway
                    //       before relaying the message to the IC
                    client_session
                        .relay_client_message(ws_open_message)
                        .instrument(client_session_span.clone())
                        .await
                        .map_err(|e| format!("Could not relay WS open message to IC: {:?}", e))?;

                    client_session_span.in_scope(|| {
                        debug!("Client session setup");
                    });

                    // check if a new poller has to be started
                    // if so, start the poller
                    if let Some(new_poller_state) = new_poller_state {
                        self.start_poller(
                            client_session
                                .canister_id
                                .expect("must be set during Setup"),
                            new_poller_state,
                        );
                    }
                    // do not return anything as the session is still alive
                },
                Ok(Some(IcWsSessionState::Open)) => {
                    client_session_span.in_scope(|| {
                        debug!("Client session opened");
                    });
                    // do not return anything as the session is still alive
                },
                Ok(Some(IcWsSessionState::Closed)) => {
                    client_session_span.in_scope(|| {
                        debug!("Client session closed");
                    });

                    // remove client from poller state
                    self.gateway_shared_state.remove_client(
                        client_session
                            .canister_id
                            .expect("must be set during Setup"),
                        client_session
                            .client_key
                            .clone()
                            .expect("must be set during Setup"),
                    );
                    // return Ok as the session was closed correctly
                    return Ok(());
                },
                Ok(None) => {
                    // no state change
                    continue;
                },
                Err(e) => {
                    if let IcWsError::Poller(e) = e {
                        // no need to remove the client as the whole poller state has already been removed by the poller task
                        return Err(format!("Poller error: {:?}", e));
                    }
                    // if the error is not due to a a failed poller
                    // remove client from poller state, if it is present
                    // error might have happened before the client session was Setup
                    // if so, there is no need to remove the client as it is not yet in the poller state
                    if self.gateway_shared_state.remove_client_if_exists(
                        client_session
                            .canister_id
                            .expect("must be set during Setup"),
                        client_session
                            .client_key
                            .clone()
                            .expect("must be set during Setup"),
                    ) {
                        // return Err as the session had an error and cannot be updated anymore
                        return Err(format!("Client session error: {:?}", e));
                    }
                    return Err(format!("Client error before session Setup: {:?}", e));
                },
            }
        }
    }

    /// Starts a new canister poller
    fn start_poller(&self, canister_id: CanisterPrincipal, poller_state: PollerState) {
        info!("Starting poller");

        // spawn new canister poller task
        let agent = Arc::clone(&self.agent);
        let gateway_shared_state = Arc::clone(&self.gateway_shared_state);
        let analyzer_channel_tx = self.analyzer_channel_tx.clone();
        let polling_interval_ms = self.polling_interval_ms;
        tokio::spawn(async move {
            // we pass both the whole gateway state and the poller state for the specific canister
            // the poller can access the poller state to determine which clients are connected
            // without having to lock the whole gateway state (TODO: check if true)
            // the poller periodically checks whether there are clients connected in the poller state and,
            // if not, removes the corresponding entry from the gateway state and terminates
            // TODO: figure out if this having the poller state actually helps
            let mut poller = CanisterPoller::new(
                agent,
                canister_id,
                poller_state,
                gateway_shared_state,
                polling_interval_ms,
                analyzer_channel_tx,
            );
            poller.run_polling().await;
            info!("Poller terminated");
            // the poller takes care of notifying the session handlers when an error is detected
            // and removing its corresponding entry from the gateway state
            // therefore, this task can simply terminate without doing anything
        });
    }
}

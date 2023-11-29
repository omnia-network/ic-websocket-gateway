use crate::{
    client_session_handler::ClientSessionHandler,
    events_analyzer::{Events, EventsCollectionType, EventsReference},
    manager::GatewaySharedState,
    metrics::ws_listener_metrics::{ListenerEvents, ListenerEventsMetrics},
};
use ic_agent::Agent;
use native_tls::Identity;
use rand::Rng;
use std::{fs, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Receiver, Sender},
    time::timeout,
};
use tokio_native_tls::{TlsAcceptor, TlsStream};
use tracing::{debug, error, info, span, warn, Instrument, Level, Span};

/// Possible TCP streams.
pub enum CustomStream {
    Tcp(TcpStream),
    TcpWithTls(TlsStream<TcpStream>),
}

/// Paths to certificate and certificate key
pub struct TlsConfig {
    pub certificate_pem_path: String,
    pub certificate_key_pem_path: String,
}

type TlsAcceptorTimeout = Duration;

/// Identifier of the client connection
pub type ClientId = u64;

/// Status of the rate limiting
#[derive(Clone)]
pub enum LimitingRateStatus {
    /// Rate limiting is enabled and the only a percentage of incoming connections are accepted
    Enabled(f64),
    /// Rate limiting is disabled
    Disabled,
}

/// Contains the information of an accepted connection needed to start a client session handler
pub struct AcceptedConnection {
    /// Identifier of the client connection
    pub client_id: ClientId,
    /// TCP stream
    pub stream: CustomStream,
    /// Events related to the connection
    pub listener_events: ListenerEvents,
    /// Tracing span of the connection
    pub span: AcceptedConnectionSpan,
}

pub type AcceptedConnectionSpan = Span;

/// Listener of incoming TCP connections
pub struct WsListener {
    // Listener of incoming TCP connectionsx
    listener: TcpListener,
    // TLS acceptor (if enabled)
    tls_acceptor: Option<TlsAcceptor>,
    /// Agent used to interact with the IC
    agent: Arc<Agent>,
    /// State of the gateway
    gateway_shared_state: GatewaySharedState,
    /// Sender side of the channel used to send events from different components to the events analyzer
    analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
    // Receiver side of the channel used to send the current limiting rate to be applied to incoming connections
    rate_limiting_channel_rx: Receiver<Option<f64>>,
    // Polling interval in milliseconds
    polling_interval_ms: u64,
    // Client ID assigned to the next client connection
    next_client_id: ClientId,
    // Status of the rate limiting
    limiting_rate_status: LimitingRateStatus,
}

impl WsListener {
    pub async fn new(
        gateway_address: &str,
        agent: Arc<Agent>,
        gateway_shared_state: GatewaySharedState,
        analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
        rate_limiting_channel_rx: Receiver<Option<f64>>,
        polling_interval_ms: u64,
        tls_config: Option<TlsConfig>,
    ) -> Self {
        let listener = TcpListener::bind(&gateway_address)
            .await
            .expect("Can't listen on this address");
        let tls_acceptor = {
            if let Some(tls_config) = tls_config {
                let chain =
                    fs::read(tls_config.certificate_pem_path).expect("Can't read certificate");
                let privkey =
                    fs::read(tls_config.certificate_key_pem_path).expect("Can't read private key");
                let tls_identity =
                    Identity::from_pkcs8(&chain, &privkey).expect("Can't create a TLS identity");
                let acceptor = TlsAcceptor::from(
                    native_tls::TlsAcceptor::builder(tls_identity)
                        .build()
                        .expect("Can't create a TLS acceptor from the TLS identity"),
                );
                info!("TLS enabled");
                Some(acceptor)
            } else {
                info!("TLS disabled");
                None
            }
        };
        Self {
            listener,
            tls_acceptor,
            agent,
            gateway_shared_state,
            analyzer_channel_tx,
            rate_limiting_channel_rx,
            polling_interval_ms,
            next_client_id: 0,
            limiting_rate_status: LimitingRateStatus::Disabled,
        }
    }

    /// Accepts incoming connections
    pub async fn listen_for_incoming_requests(&mut self) {
        // [ws listener task]        [tls acceptor task]
        // tls_acceptor_channel_rx    <----- tls_acceptor_channel_tx

        // channel used by the tls acceptor task to let the ws listener task know when the handshake is complete
        let (tls_acceptor_channel_tx, mut tls_acceptor_channel_rx): (
            Sender<AcceptedConnection>,
            Receiver<AcceptedConnection>,
        ) = mpsc::channel(100);

        loop {
            select! {
                Some(rate) = self.rate_limiting_channel_rx.recv() => {
                    match rate {
                        Some(rate) => {
                            warn!("Rate limiting {}% of incoming connections", rate*100.0);
                            self.limiting_rate_status = LimitingRateStatus::Enabled(rate);
                        },
                        None => {
                            warn!("No rate limiting applied");
                            self.limiting_rate_status = LimitingRateStatus::Disabled;
                        }
                    }
                }
                Ok((stream, client_addr)) = self.listener.accept() => {
                    if !self.must_rate_limit() {
                        self.accept_connection(
                            client_addr,
                            stream,
                            tls_acceptor_channel_tx.clone(),
                        );
                        self.next_client_id += 1;
                    } else {
                        warn!("Ignoring incoming connection due to rate limiting policy");
                    }
                },
                Some(AcceptedConnection {
                    client_id,
                    stream,
                    mut listener_events,
                    span: accept_client_connection_span
                }) = tls_acceptor_channel_rx.recv() => {
                    accept_client_connection_span.in_scope(|| {
                        // the client connection has been accepted and therefore the connection handler has to be started
                        self.start_session_handler(client_id, stream);
                    });
                    listener_events.metrics.set_started_handler();
                    self.analyzer_channel_tx.send(Box::new(listener_events)).await.expect("analyzer's side of the channel dropped");
                }
            }
        }
    }

    fn must_rate_limit(&self) -> bool {
        if let LimitingRateStatus::Enabled(limiting_rate) = self.limiting_rate_status {
            if limiting_rate < 0.0 || limiting_rate > 1.0 {
                error!(
                    "Received invalid limiting rate: {:?}. Ignoring incoming connection...",
                    limiting_rate
                );
                return true;
            }
            // receives 'limiting_rate' within [0, 1]
            // returns 'true' with probability 'limiting_rate'
            let mut rng = rand::thread_rng();
            let random_value: f64 = rng.gen(); // generate a random f64 between 0 and 1

            return random_value < limiting_rate;
        }
        false
    }

    /// Performs the TLS handshake in a separate task
    /// because it could take several seconds to complete
    /// and this would otherwise block other incoming connections
    fn accept_connection(
        &self,
        client_addr: SocketAddr,
        stream: TcpStream,
        tls_acceptor_channel_tx: Sender<AcceptedConnection>,
    ) {
        let accept_client_connection_span = span!(
            Level::DEBUG,
            "Accept Connection",
            ?client_addr,
            client_id = self.next_client_id
        );
        let client_id = self.next_client_id;
        let tls_acceptor = self.tls_acceptor.clone();
        tokio::spawn(
            async move {
                let mut listener_events = ListenerEvents::new(
                    Some(EventsReference::ClientId(client_id)),
                    EventsCollectionType::NewClientConnection,
                    ListenerEventsMetrics::default(),
                );
                listener_events.metrics.set_received_request();

                let custom_stream = match tls_acceptor {
                    Some(ref acceptor) => {
                        match timeout(TlsAcceptorTimeout::from_secs(10), acceptor.accept(stream))
                            .await
                        {
                            Ok(Ok(tls_stream)) => {
                                debug!("Accepted TLS connection");
                                listener_events.metrics.set_accepted_with_tls();
                                Ok(CustomStream::TcpWithTls(tls_stream))
                            },
                            Ok(Err(e)) => Err(format!("TLS handshake failed: {:?}", e)),
                            Err(e) => Err(format!("Accepting TLS connection timed out: {:?}", e)),
                        }
                    },
                    None => {
                        listener_events.metrics.set_accepted_without_tls();
                        debug!("Accepted connection without TLS");
                        Ok(CustomStream::Tcp(stream))
                    },
                };
                match custom_stream {
                    Ok(custom_stream) => {
                        tls_acceptor_channel_tx
                            .send(AcceptedConnection {
                                client_id,
                                stream: custom_stream,
                                listener_events,
                                span: Span::current(),
                            })
                            .await
                            .expect("ws listener's side of the channel dropped");
                    },
                    Err(e) => {
                        error!("Failed to accept connection: {:?}", e);
                    },
                }
            }
            .instrument(accept_client_connection_span),
        );
    }

    /// Spawns a new session handler
    fn start_session_handler(&self, client_id: ClientId, stream: CustomStream) {
        debug!("Spawning new connection handler");
        let client_session_handler_span =
            span!(parent: &Span::current(),Level::DEBUG, "Client Session Handler", client_id);

        let agent = Arc::clone(&self.agent);
        let gateway_shared_state = Arc::clone(&self.gateway_shared_state);
        let analyzer_channel_tx = self.analyzer_channel_tx.clone();
        let polling_interval_ms = self.polling_interval_ms;
        // spawn a session handler task for each incoming client connection
        tokio::spawn(
            async move {
                let mut client_session_handler = ClientSessionHandler::new(
                    client_id,
                    agent,
                    gateway_shared_state,
                    analyzer_channel_tx,
                    polling_interval_ms,
                );
                debug!("Started client session handler task");

                if let Err(e) = {
                    match stream {
                        CustomStream::Tcp(stream) => {
                            client_session_handler
                                .start_session(stream)
                                .instrument(Span::current())
                                .await
                        },
                        CustomStream::TcpWithTls(stream) => {
                            client_session_handler
                                .start_session(stream)
                                .instrument(Span::current())
                                .await
                        },
                    }
                } {
                    warn!("Error in client session handler: {:?}", e);
                }
                debug!("Terminated client session handler task");
            }
            .instrument(client_session_handler_span),
        );
    }
}

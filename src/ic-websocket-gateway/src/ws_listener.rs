use crate::{
    client_session_handler::ClientSessionHandler,
    events_analyzer::{Events, EventsCollectionType, EventsReference},
    manager::GatewayState,
    metrics::ws_listener_metrics::{ListenerEvents, ListenerEventsMetrics},
};
use futures_util::select_biased;
use ic_agent::Agent;
use native_tls::Identity;
use rand::Rng;
use std::{fs, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Receiver, Sender},
    time::timeout,
};
use tokio_native_tls::{TlsAcceptor, TlsStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, span, warn, Instrument, Level, Span};

/// Possible TCP streams.
pub enum CustomStream {
    Tcp(TcpStream),
    TcpWithTls(TlsStream<TcpStream>),
}

pub struct TlsConfig {
    pub certificate_pem_path: String,
    pub certificate_key_pem_path: String,
}

pub struct WsListener {
    // listener of incoming TCP connections
    listener: TcpListener,
    tls_acceptor: Option<TlsAcceptor>,
    agent: Arc<Agent>,
    gateway_state: GatewayState,
    events_channel_tx: Sender<Box<dyn Events + Send>>,
    rate_limiting_channel_rx: Receiver<Option<f64>>,
    polling_interval: u64,
    // needed to know which client_session to delete in case of error or WS closed
    next_client_id: u64,
}

impl WsListener {
    pub async fn new(
        gateway_address: &str,
        agent: Arc<Agent>,
        gateway_state: GatewayState,
        events_channel_tx: Sender<Box<dyn Events + Send>>,
        rate_limiting_channel_rx: Receiver<Option<f64>>,
        polling_interval: u64,
        tls_config: Option<TlsConfig>,
    ) -> Self {
        let listener = TcpListener::bind(&gateway_address)
            .await
            .expect("Can't listen");
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
            gateway_state,
            events_channel_tx,
            rate_limiting_channel_rx,
            polling_interval,
            next_client_id: 0,
        }
    }

    pub async fn listen_for_incoming_requests(&mut self) {
        // [ws listener task]        [tls acceptor task]
        // tls_acceptor_rx    <----- tls_acceptor_tx

        // channel used by the tls acceptor task to let the ws listener task know when the handshake is complete
        let (tls_acceptor_tx, mut tls_acceptor_rx): (
            Sender<Result<(u64, CustomStream, ListenerEvents, Span), String>>,
            Receiver<Result<(u64, CustomStream, ListenerEvents, Span), String>>,
        ) = mpsc::channel(1000);

        let mut limiting_rate: f64 = 0.0;
        loop {
            select! {
                Some(rate) = self.rate_limiting_channel_rx.recv() => {
                    match rate {
                        Some(rate) => {
                            warn!("Rate limiting {}% of incoming connections", rate*100.0);
                            limiting_rate = rate;
                        },
                        None => {
                            warn!("No rate limiting applied");
                            limiting_rate = 0.0;
                        }
                    }
                }
                Ok((stream, client_addr)) = self.listener.accept() => {
                    if !is_in_rate_limit(limiting_rate) {
                        accept_connection(
                            self.next_client_id,
                            client_addr,
                            stream,
                            self.tls_acceptor.clone(),
                            tls_acceptor_tx.clone()
                        );
                        self.next_client_id += 1;
                    } else {
                        warn!("Ignoring incoming connection due to rate limiting policy");
                    }
                },
                Some(Ok((current_client_id , stream, mut listener_events, accept_client_connection_span))) = tls_acceptor_rx.recv() => {
                    // the client connection has been accepted and therefore the connection handler has to be started
                    self.start_session_handler(current_client_id, stream, accept_client_connection_span);
                    listener_events.metrics.set_started_handler();
                    self.events_channel_tx.send(Box::new(listener_events)).await.expect("analyzer's side of the channel dropped");
                }
            }
        }
    }

    fn start_session_handler(
        &self,
        client_id: u64,
        stream: CustomStream,
        accept_client_connection_span: Span,
    ) {
        accept_client_connection_span.in_scope(|| {
            debug!("Spawning new connection handler");
        });
        let client_connection_span = span!(Level::DEBUG, "Client Connection", client_id);
        client_connection_span.follows_from(accept_client_connection_span.id());

        let agent = Arc::clone(&self.agent);
        let gateway_state = Arc::clone(&self.gateway_state);
        let events_channel_tx = self.events_channel_tx.clone();
        // spawn a connection handler task for each incoming client connection
        tokio::spawn(
            async move {
                let mut client_session_handler =
                    ClientSessionHandler::new(client_id, agent, gateway_state, events_channel_tx);
                match stream {
                    CustomStream::Tcp(stream) => client_session_handler.start_session(stream).await,
                    CustomStream::TcpWithTls(stream) => {
                        client_session_handler.start_session(stream).await
                    },
                }
            }
            .instrument(client_connection_span),
        );
    }
}

fn is_in_rate_limit(limiting_rate: f64) -> bool {
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

    random_value < limiting_rate
}

/// the TLS handshake is performed in a separate task because it could take several seconds to complete and this would otherwise block other incoming connections
pub fn accept_connection(
    client_id: u64,
    client_addr: std::net::SocketAddr,
    stream: TcpStream,
    tls_acceptor: Option<TlsAcceptor>,
    tls_acceptor_tx: Sender<Result<(u64, CustomStream, ListenerEvents, Span), String>>,
) {
    let accept_client_connection_span =
        span!(Level::DEBUG, "Accept Connection", ?client_addr, client_id);

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
                    match timeout(Duration::from_secs(10), acceptor.accept(stream)).await {
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
                    tls_acceptor_tx
                        .send(Ok((
                            client_id,
                            custom_stream,
                            listener_events,
                            Span::current(),
                        )))
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

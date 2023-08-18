use crate::{
    client_connection_handler::{ClientConnectionHandler, WsConnectionState},
    metrics_analyzer::{Deltas, Metrics, MetricsReference, TimeableEvent},
};

use ic_agent::Agent;
use native_tls::Identity;
use std::{fs, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::Sender,
};
use tokio_native_tls::{TlsAcceptor, TlsStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, span, Instrument, Level, Span};

/// Possible TCP streams.
enum CustomStream {
    Tcp(TcpStream),
    TcpWithTls(TlsStream<TcpStream>),
}

pub struct TlsConfig {
    pub certificate_pem_path: String,
    pub certificate_key_pem_path: String,
}

#[derive(Debug)]
struct ListenerMetrics {
    reference: Option<MetricsReference>,
    received_request: TimeableEvent,
    accepted_with_tls: TimeableEvent,
    accepted_without_tls: TimeableEvent,
    started_handler: TimeableEvent,
}

impl ListenerMetrics {
    fn new(id: u64) -> Self {
        let reference = Some(MetricsReference::ClientId(id));
        Self {
            reference,
            received_request: TimeableEvent::default(),
            accepted_with_tls: TimeableEvent::default(),
            accepted_without_tls: TimeableEvent::default(),
            started_handler: TimeableEvent::default(),
        }
    }

    fn set_received_request(&mut self) {
        self.received_request.set_now()
    }

    fn set_accepted_with_tls(&mut self) {
        self.accepted_with_tls.set_now()
    }

    fn set_accepted_without_tls(&mut self) {
        self.accepted_without_tls.set_now()
    }

    fn set_started_handler(&mut self) {
        self.started_handler.set_now()
    }
}

impl Metrics for ListenerMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.received_request
    }

    fn get_reference(&self) -> &Option<MetricsReference> {
        &self.reference
    }

    fn compute_deltas(&self) -> Option<Box<dyn Deltas + Send>> {
        let accepted = {
            if self.accepted_with_tls.is_set() {
                self.accepted_with_tls.clone()
            } else {
                self.accepted_without_tls.clone()
            }
        };
        let time_to_accept = accepted.duration_since(&self.received_request)?;
        let time_to_start_handling = self.started_handler.duration_since(&accepted)?;
        let latency = self.compute_latency()?;

        Some(Box::new(ListenerDeltas::new(
            self.reference.clone(),
            time_to_accept,
            time_to_start_handling,
            latency,
        )))
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.started_handler.duration_since(&self.received_request)
    }
}

#[derive(Debug)]
struct ListenerDeltas {
    reference: Option<MetricsReference>,
    time_to_accept: Duration,
    time_to_start_handling: Duration,
    latency: Duration,
}

impl ListenerDeltas {
    fn new(
        reference: Option<MetricsReference>,
        time_to_accept: Duration,
        time_to_start_handling: Duration,
        latency: Duration,
    ) -> Self {
        Self {
            reference,
            time_to_accept,
            time_to_start_handling,
            latency,
        }
    }
}

impl Deltas for ListenerDeltas {
    fn display(&self) {
        debug!(
            "\nreference: {:?}\ntime_to_accept: {:?}\ntime_to_start_handling: {:?}\nlatency: {:?}",
            self.reference, self.time_to_accept, self.time_to_start_handling, self.latency
        );
    }

    fn get_latency(&self) -> Duration {
        self.latency
    }
}
pub struct WsListener {
    // listener of incoming TCP connections
    listener: TcpListener,
    tls_acceptor: Option<TlsAcceptor>,
    agent: Arc<Agent>,
    client_connection_handler_tx: Sender<WsConnectionState>,
    metrics_channel_tx: Sender<Box<dyn Metrics + Send>>,
    // needed to know which gateway_session to delete in case of error or WS closed
    next_client_id: u64,
}

impl WsListener {
    pub async fn new(
        gateway_address: &str,
        agent: Arc<Agent>,
        client_connection_handler_tx: Sender<WsConnectionState>,
        metrics_channel_tx: Sender<Box<dyn Metrics + Send>>,
        tls_config: Option<TlsConfig>,
    ) -> Self {
        let listener = TcpListener::bind(&gateway_address)
            .await
            .expect("Can't listen");
        let mut tls_acceptor = None;
        if let Some(tls_config) = tls_config {
            let chain = fs::read(tls_config.certificate_pem_path).expect("Can't read certificate");
            let privkey =
                fs::read(tls_config.certificate_key_pem_path).expect("Can't read private key");
            let tls_identity =
                Identity::from_pkcs8(&chain, &privkey).expect("Can't create a TLS identity");
            let acceptor = TlsAcceptor::from(
                native_tls::TlsAcceptor::builder(tls_identity)
                    .build()
                    .expect("Can't create a TLS acceptor from the TLS identity"),
            );
            tls_acceptor = Some(acceptor);
            info!("TLS enabled");
        } else {
            info!("TLS disabled");
        }
        Self {
            listener,
            tls_acceptor,
            agent,
            client_connection_handler_tx,
            metrics_channel_tx,
            next_client_id: 0,
        }
    }

    pub async fn listen_for_incoming_requests(&mut self, parent_token: CancellationToken) {
        // needed to ensure that we stop listening for incoming requests before we start shutting down the connections
        let child_token = CancellationToken::new();

        let wait_for_cancellation = parent_token.cancelled();
        tokio::pin!(wait_for_cancellation);
        loop {
            select! {
                // bias select! to check token cancellation first
                // with 'biased', async functions are polled in the order in which they appear
                biased;
                _ = &mut wait_for_cancellation => {
                    child_token.cancel();
                    info!("Stopped listening for incoming requests");
                    break;
                },
                Ok((stream, client_addr)) = self.listener.accept() => {
                    let current_client_id = self.next_client_id;
                    let mut listener_metrics = ListenerMetrics::new(current_client_id);
                    listener_metrics.set_received_request();
                    let span = span!(
                        Level::INFO,
                        "handle_client_connection",
                        client_addr = ?client_addr,
                        client_id = current_client_id
                    );
                    let _guard = span.enter();

                    let stream = match self.tls_acceptor {
                        Some(ref acceptor) => {
                            let tls_stream = acceptor.accept(stream).await;
                            match tls_stream {
                                Ok(tls_stream) => {
                                    debug!("TLS handshake successful");
                                    listener_metrics.set_accepted_with_tls();
                                    CustomStream::TcpWithTls(tls_stream)
                                },
                                Err(e) => {
                                    error!("TLS handshake failed: {:?}", e);
                                    continue;
                                },
                            }
                        },
                        None => {
                            listener_metrics.set_accepted_without_tls();
                            CustomStream::Tcp(stream)
                        },
                    };

                    self.start_connection_handler(stream, current_client_id, child_token.clone(), span.clone());
                    self.next_client_id += 1;

                    listener_metrics.set_started_handler();
                    self.metrics_channel_tx.send(Box::new(listener_metrics)).await.expect("analyzer's side of the channel dropped")
                },
            }
        }
    }

    fn start_connection_handler(
        &self,
        stream: CustomStream,
        client_id: u64,
        token: CancellationToken,
        span: Span,
    ) {
        let agent = Arc::clone(&self.agent);
        let client_connection_handler_tx = self.client_connection_handler_tx.clone();
        let metrics_channel_tx = self.metrics_channel_tx.clone();
        // spawn a connection handler task for each incoming client connection
        tokio::spawn(
            async move {
                let client_connection_handler = ClientConnectionHandler::new(
                    client_id,
                    agent,
                    client_connection_handler_tx,
                    metrics_channel_tx,
                    token,
                );
                debug!("Spawned new connection handler");
                match stream {
                    CustomStream::Tcp(stream) => {
                        client_connection_handler.handle_stream(stream).await
                    },
                    CustomStream::TcpWithTls(stream) => {
                        client_connection_handler.handle_stream(stream).await
                    },
                }
                debug!("Terminated client connection handler task");
            }
            .instrument(span),
        );
    }
}

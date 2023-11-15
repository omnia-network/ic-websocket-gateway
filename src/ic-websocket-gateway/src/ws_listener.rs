use crate::{
    client_connection_handler::{ClientConnectionHandler, IcWsConnectionState},
    events_analyzer::{Events, EventsCollectionType, EventsReference},
    metrics::ws_listener_metrics::{ListenerEvents, ListenerEventsMetrics},
};

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
    client_connection_handler_tx: Sender<IcWsConnectionState>,
    events_channel_tx: Sender<Box<dyn Events + Send>>,
    rate_limiting_channel_rx: Receiver<Option<f64>>,
    // needed to know which client_session to delete in case of error or WS closed
    next_client_id: u64,
}

impl WsListener {
    pub async fn new(
        gateway_address: &str,
        agent: Arc<Agent>,
        client_connection_handler_tx: Sender<IcWsConnectionState>,
        events_channel_tx: Sender<Box<dyn Events + Send>>,
        rate_limiting_channel_rx: Receiver<Option<f64>>,
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
            client_connection_handler_tx,
            events_channel_tx,
            rate_limiting_channel_rx,
            next_client_id: 0,
        }
    }

    pub async fn listen_for_incoming_requests(&mut self, parent_token: CancellationToken) {
        // needed to ensure that we stop listening for incoming requests before we start shutting down the connections
        let child_token = CancellationToken::new();

        let wait_for_cancellation = parent_token.cancelled();
        tokio::pin!(wait_for_cancellation);

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
                // bias select! to check token cancellation first
                // with 'biased', async functions are polled in the order in which they appear
                biased;
                _ = &mut wait_for_cancellation => {
                    child_token.cancel();
                    info!("Stopped listening for incoming requests");
                    break;
                },
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
                Some(tls_acceptor_result) = tls_acceptor_rx.recv() => {
                    // a client completed the TLS handshake and therefore the connection hanlder has to be started
                    match tls_acceptor_result {
                        Ok((current_client_id , stream, mut listener_events, span)) => {
                            self.start_connection_handler(stream, current_client_id, child_token.clone(), span);
                            listener_events.metrics.set_started_handler();
                            self.events_channel_tx.send(Box::new(listener_events)).await.expect("analyzer's side of the channel dropped")
                        },
                        Err(e) => error!("{:?}", e)
                    }
                }
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
        let events_channel_tx = self.events_channel_tx.clone();
        // spawn a connection handler task for each incoming client connection
        tokio::spawn(
            async move {
                let mut client_connection_handler = ClientConnectionHandler::new(
                    client_id,
                    agent,
                    client_connection_handler_tx,
                    events_channel_tx,
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
    current_client_id: u64,
    client_addr: std::net::SocketAddr,
    stream: TcpStream,
    tls_acceptor: Option<TlsAcceptor>,
    tls_acceptor_tx: Sender<Result<(u64, CustomStream, ListenerEvents, Span), String>>,
) {
    let span = span!(
        Level::INFO,
        "handle_client_connection",
        client_addr = ?client_addr,
        client_id = current_client_id
    );

    tokio::spawn(
        async move {
            let mut listener_events = ListenerEvents::new(
                Some(EventsReference::ClientId(current_client_id)),
                EventsCollectionType::NewClientConnection,
                ListenerEventsMetrics::default(),
            );
            listener_events.metrics.set_received_request();

            let res = match tls_acceptor {
                Some(ref acceptor) => {
                    match timeout(Duration::from_secs(10), acceptor.accept(stream)).await {
                        Ok(Ok(tls_stream)) => {
                            debug!("TLS handshake successful");
                            listener_events.metrics.set_accepted_with_tls();
                            Ok((
                                current_client_id,
                                CustomStream::TcpWithTls(tls_stream),
                                listener_events,
                                Span::current(),
                            ))
                        },
                        Ok(Err(e)) => Err(format!("TLS handshake failed: {:?}", e)),
                        Err(e) => Err(format!("Accepting TLS connection timed out: {:?}", e)),
                    }
                },
                None => {
                    listener_events.metrics.set_accepted_without_tls();
                    debug!("Accepted connection without TLS");
                    Ok((
                        current_client_id,
                        CustomStream::Tcp(stream),
                        listener_events,
                        Span::current(),
                    ))
                },
            };
            tls_acceptor_tx
                .send(res)
                .await
                .expect("ws listener's side of the channel dropped");
        }
        .instrument(span),
    );
}

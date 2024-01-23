use crate::client_session_handler::ClientSessionHandler;
use gateway_state::GatewayState;
use ic_agent::Agent;
use native_tls::Identity;
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

/// Contains the information of an accepted connection needed to start a client session handler
pub struct AcceptedConnection {
    /// Identifier of the client connection
    pub client_id: ClientId,
    /// TCP stream
    pub stream: CustomStream,
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
    gateway_state: GatewayState,
    // Polling interval in milliseconds
    polling_interval_ms: u64,
    // Client ID assigned to the next client connection
    next_client_id: ClientId,
}

impl WsListener {
    pub async fn new(
        gateway_address: &str,
        agent: Arc<Agent>,
        gateway_state: GatewayState,
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
            gateway_state,
            polling_interval_ms,
            next_client_id: 0,
        }
    }

    /// Accepts incoming connections
    pub async fn listen_for_incoming_requests(&mut self) {
        // [ws listener task]                [tls acceptor task]
        // tls_acceptor_channel_rx    <----- tls_acceptor_channel_tx

        // channel used by the tls acceptor task to let the ws listener task know when the handshake is complete
        let (tls_acceptor_channel_tx, mut tls_acceptor_channel_rx): (
            Sender<AcceptedConnection>,
            Receiver<AcceptedConnection>,
        ) = mpsc::channel(100);

        loop {
            select! {
                Ok((stream, client_addr)) = self.listener.accept() => {
                    self.accept_connection(
                        client_addr,
                        stream,
                        tls_acceptor_channel_tx.clone(),
                    );
                    self.next_client_id += 1;
                },
                Some(AcceptedConnection {
                    client_id,
                    stream,
                    span: accept_client_connection_span
                }) = tls_acceptor_channel_rx.recv() => {
                    accept_client_connection_span.in_scope(|| {
                        // the client connection has been accepted and therefore the connection handler has to be started
                        self.start_session_handler(client_id, stream);
                    });
                }
            }
        }
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
            client_addr = ?client_addr.ip(),
            client_id = self.next_client_id,
            cargo_version = env!("CARGO_PKG_VERSION"),
        );
        let client_id = self.next_client_id;
        let tls_acceptor = self.tls_acceptor.clone();
        tokio::spawn(
            async move {
                let custom_stream = match tls_acceptor {
                    Some(ref acceptor) => {
                        match timeout(TlsAcceptorTimeout::from_secs(10), acceptor.accept(stream))
                            .await
                        {
                            Ok(Ok(tls_stream)) => {
                                debug!("Accepted TLS connection");
                                Ok(CustomStream::TcpWithTls(tls_stream))
                            },
                            Ok(Err(e)) => Err(format!("TLS handshake failed: {:?}", e)),
                            Err(e) => Err(format!("Accepting TLS connection timed out: {:?}", e)),
                        }
                    },
                    None => {
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
        let gateway_state = self.gateway_state.clone();
        let polling_interval_ms = self.polling_interval_ms;
        // spawn a session handler task for each incoming client connection
        tokio::spawn(
            async move {
                let mut client_session_handler =
                    ClientSessionHandler::new(client_id, agent, gateway_state, polling_interval_ms);
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

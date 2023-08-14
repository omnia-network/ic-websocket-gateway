use crate::client_connection_handler::{ClientConnectionHandler, WsConnectionState};

use ic_agent::Agent;
use native_tls::Identity;
use std::{fs, sync::Arc};
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

pub struct WsListener {
    // listener of incoming TCP connections
    listener: TcpListener,
    tls_acceptor: Option<TlsAcceptor>,
    agent: Arc<Agent>,
    client_connection_handler_tx: Sender<WsConnectionState>,
    // needed to know which gateway_session to delete in case of error or WS closed
    next_client_id: u64,
}

impl WsListener {
    pub async fn new(
        gateway_address: &str,
        agent: Arc<Agent>,
        client_connection_handler_tx: Sender<WsConnectionState>,
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
                                    CustomStream::TcpWithTls(tls_stream)
                                },
                                Err(e) => {
                                    error!("TLS handshake failed: {:?}", e);
                                    continue;
                                },
                            }
                        },
                        None => {
                            CustomStream::Tcp(stream)
                        },
                    };

                    self.start_connection_handler(stream, current_client_id, child_token.clone(), span.clone());

                    self.next_client_id += 1;
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
        // spawn a connection handler task for each incoming client connection
        tokio::spawn(
            async move {
                let client_connection_handler = ClientConnectionHandler::new(
                    client_id,
                    agent,
                    client_connection_handler_tx,
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

use crate::ws_listener::TlsConfig;
use crate::{events_analyzer::EventsAnalyzer, gateway_server::GatewayServer};
use bytes::Bytes;
use futures::{
    future::{self, Ready},
    Future,
};
use http::{Method, Request, Response, StatusCode};
use hyper::{server::conn::AddrStream, Body, Server};
use ic_identity::{get_identity_from_key_pair, load_key_pair};
use std::error::Error;
use std::{
    fs::{self, File},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};
use structopt::StructOpt;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tower::{Service, ServiceBuilder};
use tracing::{self, error, info, trace};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{filter::EnvFilter, reload::Handle};

mod canister_methods;
mod canister_poller;
mod client_connection_handler;
mod events_analyzer;
mod gateway_server;
mod messages_demux;
mod ws_listener;
mod metrics {
    pub mod canister_poller_metrics;
    pub mod client_connection_handler_metrics;
    pub mod gateway_server_metrics;
    pub mod ws_listener_metrics;
}
mod tests {
    pub mod canister_poller;
    pub mod client_connection_handler;
    pub mod events_analyzer;
}

#[derive(Debug, StructOpt)]
#[structopt(name = "Gateway", about = "IC WS Gateway")]
struct DeploymentInfo {
    #[structopt(long, default_value = "http://127.0.0.1:4943")]
    /// can be set by running: 'cargo run -- --subnet-url=http://localhost:4943'
    subnet_url: String,

    #[structopt(long, default_value = "0.0.0.0:8080")]
    /// address at which the WebSocket Gateway is reachable
    gateway_address: String,

    #[structopt(long, default_value = "100")]
    /// time interval at which the canister is polled
    polling_interval: u64,

    #[structopt(long, default_value = "100")]
    /// minimum interval between incoming messages
    /// if below this threshold, the gateway starts rate liimiting
    min_incoming_interval: u64,

    #[structopt(long, default_value = "10")]
    /// threshold after which the metrics analyzer computes the averages of the intervals/latencies
    compute_averages_threshold: u64,

    #[structopt(long)]
    tls_certificate_pem_path: Option<String>,

    #[structopt(long)]
    tls_certificate_key_pem_path: Option<String>,
}

type Err = Box<dyn Error + Send + Sync + 'static>;

struct AdminSvc<S> {
    handle: Handle<EnvFilter, S>,
}

impl<S> Clone for AdminSvc<S> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
        }
    }
}

impl<'a, S> Service<&'a AddrStream> for AdminSvc<S>
where
    S: tracing::Subscriber,
{
    type Response = AdminSvc<S>;
    type Error = hyper::Error;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: &'a AddrStream) -> Self::Future {
        future::ok(self.clone())
    }
}

impl<S> Service<Request<Body>> for AdminSvc<S>
where
    S: tracing::Subscriber + 'static,
{
    type Response = Response<Body>;
    type Error = Err;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Body>, Err>> + std::marker::Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        // we need to clone so that the reference to self
        // isn't outlived by the returned future.
        let handle = self.clone();
        let f = async move {
            let rsp = match (req.method(), req.uri().path()) {
                (&Method::PUT, "/filter") => {
                    trace!("setting filter");

                    let body = hyper::body::to_bytes(req).await?;
                    match handle.set_from(body) {
                        Err(error) => {
                            error!(%error, "setting filter failed!");
                            rsp(StatusCode::INTERNAL_SERVER_ERROR, error)
                        },
                        Ok(()) => rsp(StatusCode::NO_CONTENT, Body::empty()),
                    }
                },
                _ => rsp(StatusCode::NOT_FOUND, "try `/filter`"),
            };
            Ok(rsp)
        };
        Box::pin(f)
    }
}

impl<S> AdminSvc<S>
where
    S: tracing::Subscriber + 'static,
{
    fn set_from(&self, bytes: Bytes) -> Result<(), String> {
        use std::str;
        let body = str::from_utf8(bytes.as_ref()).map_err(|e| format!("{}", e))?;
        trace!(request.body = ?body);
        let new_filter = body
            .parse::<tracing_subscriber::filter::EnvFilter>()
            .map_err(|e| format!("{}", e))?;
        self.handle.reload(new_filter).map_err(|e| format!("{}", e))
    }
}

fn rsp(status: StatusCode, body: impl Into<Body>) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(body.into())
        .expect("builder with known status code must not fail")
}

fn create_data_dir() -> Result<(), String> {
    if !Path::new("./data").is_dir() {
        fs::create_dir("./data").map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn init_tracing(gateway_address: &str) -> Result<(WorkerGuard, WorkerGuard), String> {
    if !Path::new("./data/traces").is_dir() {
        fs::create_dir("./data/traces").map_err(|e| e.to_string())?;
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    let filename = format!("./data/traces/gateway_{:?}.log", timestamp.as_millis());

    println!("Tracing to file: {}", filename);

    let log_file = File::create(filename).map_err(|e| e.to_string())?;
    let (_non_blocking_file, guard_file) = tracing_appender::non_blocking(log_file);
    let (non_blocking_stdout, guard_stdout) = tracing_appender::non_blocking(std::io::stdout());

    // let env_filter_file = EnvFilter::builder()
    //     .with_env_var("RUST_LOG_FILE")
    //     .try_from_env()
    //     .unwrap_or_else(|_| EnvFilter::new("ic_websocket_gateway=trace"));
    // let file_tracing_layer = tracing_subscriber::fmt::layer()
    //     .json()
    //     .with_writer(non_blocking_file)
    //     .with_thread_ids(true)
    //     .with_filter(env_filter_file);

    let env_filter_stdout = EnvFilter::builder()
        .with_env_var("RUST_LOG_STDOUT")
        .try_from_env()
        .unwrap_or_else(|_| EnvFilter::new("ic_websocket_gateway=info"));
    let stdout_tracing_layer = tracing_subscriber::fmt()
        .with_writer(non_blocking_stdout)
        .pretty()
        .with_env_filter(env_filter_stdout)
        // it is possible to dinamically reconfigure the filter with the following command:
        // :; curl -X PUT localhost:3001/filter -d "ic_websocket_gateway=error"
        //
        .with_filter_reloading();

    let handle = stdout_tracing_layer.reload_handle();
    stdout_tracing_layer.init();

    let admin_addr = gateway_address[..gateway_address.len() - 5].to_string() + ":3001";
    let admin_addr = admin_addr
        .parse::<SocketAddr>()
        .map_err(|e| e.to_string())?;
    let admin = ServiceBuilder::new().service(AdminSvc { handle });
    let admin = Server::bind(&admin_addr).serve(admin);
    info!(
        "Admin server for dynamic tracing filter configuration listening on {}",
        admin_addr
    );

    tokio::spawn(admin);
    // tracing_subscriber::registry()
    //     .with(file_tracing_layer)
    //     .init();

    Ok((guard_file, guard_stdout))
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let deployment_info = DeploymentInfo::from_args();
    info!("Deployment info: {:?}", deployment_info);

    create_data_dir()?;
    let _guards = init_tracing(&deployment_info.gateway_address).expect("could not init tracing");

    let key_pair = load_key_pair("./data/key_pair")?;
    let identity = get_identity_from_key_pair(key_pair);

    // [any task]               [events analyzer task]
    // events_channel_tx -----> events_channel_rx

    // channel used to send events to the events analyzer which groups and processes them
    let (events_channel_tx, events_channel_rx) = mpsc::channel(100);

    // [events analyzer task]          [ws_listener]
    // rate_limiting_channel_tx -----> rate_limiting_channel_rx

    // channel used by the events analyzer to send the percentage of connections that should be ignored by the WS listener
    // due to the rate limiting policy
    let (rate_limiting_channel_tx, rate_limiting_channel_rx): (
        Sender<Option<f64>>,
        Receiver<Option<f64>>,
    ) = mpsc::channel(10);

    let mut gateway_server = GatewayServer::new(
        deployment_info.gateway_address,
        deployment_info.subnet_url,
        identity,
        events_channel_tx,
    )
    .await;

    let tls_config = if deployment_info.tls_certificate_pem_path.is_some()
        && deployment_info.tls_certificate_key_pem_path.is_some()
    {
        Some(TlsConfig {
            certificate_pem_path: deployment_info.tls_certificate_pem_path.unwrap(),
            certificate_key_pem_path: deployment_info.tls_certificate_key_pem_path.unwrap(),
        })
    } else {
        None
    };

    tokio::spawn(async move {
        let mut events_analyzer = EventsAnalyzer::new(
            events_channel_rx,
            rate_limiting_channel_tx,
            deployment_info.min_incoming_interval,
            deployment_info.compute_averages_threshold,
        );
        events_analyzer.start_processing().await;
    });

    // spawn a task which keeps accepting incoming connection requests from WebSocket clients
    gateway_server.start_accepting_incoming_connections(tls_config, rate_limiting_channel_rx);

    // maintains the WS Gateway state of the main task in sync with the spawned tasks
    gateway_server
        .manage_state(deployment_info.polling_interval)
        .await;
    info!("Terminated state manager");

    Ok(())
}

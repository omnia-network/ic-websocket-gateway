use crate::gateway_tracing::{init_tracing, InitTracingResult};
use crate::ws_listener::TlsConfig;
use crate::{events_analyzer::EventsAnalyzer, gateway_server::GatewayServer};
use ic_identity::{get_identity_from_key_pair, load_key_pair};
use std::{
    fs::{self},
    path::Path,
};
use structopt::StructOpt;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::info;

mod canister_methods;
mod canister_poller;
mod client_connection_handler;
mod events_analyzer;
mod gateway_server;
mod gateway_tracing;
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
    /// the URL of the IC network
    ic_network_url: String,

    #[structopt(long, default_value = "0.0.0.0:8080")]
    /// address at which the WebSocket Gateway is reachable
    gateway_address: String,

    #[structopt(long, default_value = "100")]
    /// time interval (in milliseconds) at which the canister is polled
    polling_interval: u64,

    #[structopt(long, default_value = "100")]
    /// minimum interval (in milliseconds) between incoming messages
    /// if below this threshold, the gateway starts rate limiting
    min_incoming_interval: u64,

    #[structopt(long, default_value = "10")]
    /// threshold after which the metrics analyzer computes the averages of the intervals/latencies
    compute_averages_threshold: u64,

    #[structopt(long)]
    tls_certificate_pem_path: Option<String>,

    #[structopt(long)]
    tls_certificate_key_pem_path: Option<String>,

    #[structopt(long)]
    /// Jaeger agent endpoint for the telemetry in the format <host>:<port>
    ///
    /// To run a Jaeger instance that listens on port 6831:
    /// ```bash
    /// docker run -d -p6831:6831/udp -p6832:6832/udp -p16686:16686 -p14268:14268 jaegertracing/all-in-one:latest
    /// ```
    telemetry_jaeger_agent_endpoint: Option<String>,
}

fn create_data_dir() -> Result<(), String> {
    if !Path::new("./data").is_dir() {
        fs::create_dir("./data").map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), String> {
    create_data_dir()?;
    let deployment_info = DeploymentInfo::from_args();
    let InitTracingResult {
        guards: _guards,
        is_telemetry_enabled,
    } = init_tracing(deployment_info.telemetry_jaeger_agent_endpoint.to_owned())
        .expect("could not init tracing");

    info!("Deployment info: {:?}", deployment_info);

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
        deployment_info.ic_network_url,
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

    if is_telemetry_enabled {
        opentelemetry::global::shutdown_tracer_provider();
    }

    Ok(())
}

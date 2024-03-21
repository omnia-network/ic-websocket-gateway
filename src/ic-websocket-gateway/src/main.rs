use crate::{
    gateway_metrics::init_metrics,
    gateway_tracing::{init_tracing, InitTracingResult},
    manager::Manager,
    ws_listener::TlsConfig,
};
use ic_identity::{get_identity_from_key_pair, load_key_pair};
use std::{fs, path::Path};
use structopt::StructOpt;
use tracing::info;

mod canister_poller;
mod client_session;
mod client_session_handler;
mod gateway_metrics;
mod gateway_tracing;
mod manager;
mod ws_listener;

mod tests {
    mod canister_poller;
}

#[derive(Debug, StructOpt)]
#[structopt(name = "Gateway", about = "IC WS Gateway")]
struct DeploymentInfo {
    #[structopt(long, default_value = "http://127.0.0.1:4943")]
    /// The URL of the IC network. For mainnet, use `https://icp-api.io`.
    ic_network_url: String,

    #[structopt(long, default_value = "0.0.0.0:8080")]
    /// Address at which the WebSocket Gateway is reachable.
    gateway_address: String,

    #[structopt(long, default_value = "100")]
    /// Time interval (in milliseconds) at which the canisters are polled.
    polling_interval: u64,

    #[structopt(long, default_value = "0.0.0.0:9000")]
    /// Prometheus endpoint on which the metrics are exposed.
    prometheus_endpoint: String,

    #[structopt(long)]
    tls_certificate_pem_path: Option<String>,

    #[structopt(long)]
    tls_certificate_key_pem_path: Option<String>,

    #[structopt(long)]
    /// OpenTelemetry collector endpoint for the telemetry.
    opentelemetry_collector_endpoint: Option<String>,
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
    let key_pair = load_key_pair("./data/key_pair")?;
    let identity = get_identity_from_key_pair(key_pair);

    let deployment_info = DeploymentInfo::from_args();

    let manager = Manager::new(
        deployment_info.gateway_address.clone(),
        deployment_info.ic_network_url.clone(),
        identity,
    )
    .await;

    let gateway_principal = manager.get_agent_principal();
    let InitTracingResult {
        guards: _guards,
        is_telemetry_enabled,
    } = init_tracing(
        deployment_info.opentelemetry_collector_endpoint.to_owned(),
        gateway_principal,
    )
    .expect("could not init tracing");

    init_metrics(&deployment_info.prometheus_endpoint).expect("could not init metrics");

    // must be printed after initializing tracing to ensure that the info are captured
    info!("Deployment info: {:?}", deployment_info);
    info!("Cargo version: {}", env!("CARGO_PKG_VERSION"));
    info!("Gateway Agent principal: {}", gateway_principal);

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

    // keep accept incoming client connections
    let accept_connections_handle =
        manager.start_accepting_incoming_connections(tls_config, deployment_info.polling_interval);

    tokio::join!(accept_connections_handle)
        .0
        .expect("could not join accept connections task");

    info!("Terminated gateway manager");

    if is_telemetry_enabled {
        opentelemetry::global::shutdown_tracer_provider();
    }

    Ok(())
}

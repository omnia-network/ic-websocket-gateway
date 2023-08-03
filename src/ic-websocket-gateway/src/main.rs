use gateway_server::GatewayServer;
use ic_identity::{get_identity_from_key_pair, load_key_pair};
use tracing::{info, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{filter, prelude::*};

use std::{
    fs::{self, File},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use structopt::StructOpt;

use crate::client_connection_handler::TlsConfig;

mod canister_methods;
mod canister_poller;
mod client_connection_handler;
mod gateway_server;
mod unit_tests;

#[derive(Debug, StructOpt)]
#[structopt(name = "Gateway", about = "IC WS Gateway")]
struct DeploymentInfo {
    #[structopt(short, long, default_value = "http://127.0.0.1:4943")]
    subnet_url: String,

    #[structopt(short, long, default_value = "0.0.0.0:8080")]
    gateway_address: String,

    #[structopt(short, long, default_value = "100")]
    polling_interval: u64,

    #[structopt(long)]
    tls_certificate_pem_path: Option<String>,

    #[structopt(long)]
    tls_certificate_key_pem_path: Option<String>,
}

fn init_tracing() -> Result<(WorkerGuard, WorkerGuard), String> {
    if !Path::new("./data/traces").is_dir() {
        fs::create_dir("./data/traces").map_err(|e| e.to_string())?;
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    let filename = format!("./data/traces/gateway_{:?}.log", timestamp.as_millis());

    println!("Tracing to file: {}", filename);

    let log_file = File::create(filename).map_err(|e| e.to_string())?;
    let (non_blocking_file, guard_file) = tracing_appender::non_blocking(log_file);
    let (non_blocking_stdout, guard_stdout) = tracing_appender::non_blocking(std::io::stdout());
    let debug_log_file = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_file)
        .with_thread_ids(true)
        .pretty();
    let debug_log_stdout = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_stdout)
        .pretty();
    tracing_subscriber::registry()
        .with(debug_log_file.with_filter(filter::LevelFilter::INFO))
        .with(debug_log_stdout.with_filter(filter::LevelFilter::INFO))
        .init();

    Ok((guard_file, guard_stdout))
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
    let _guards = init_tracing();

    let deployment_info = DeploymentInfo::from_args();
    info!("Deployment info: {:?}", deployment_info);

    let key_pair = load_key_pair("./data/key_pair")?;
    let identity = get_identity_from_key_pair(key_pair);

    let mut gateway_server = GatewayServer::new(
        &deployment_info.gateway_address,
        &deployment_info.subnet_url,
        identity,
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

    // spawn a task which keeps accepting incoming connection requests from WebSocket clients
    gateway_server.start_accepting_incoming_connections(tls_config);

    // maintains the WS Gateway state of the main task in sync with the spawned tasks
    gateway_server
        .manage_state(deployment_info.polling_interval)
        .await;
    warn!("Terminated state manager");

    Ok(())
}

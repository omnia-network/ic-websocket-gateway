use crate::gateway_server::GatewayServer;
use crate::ws_listener::TlsConfig;
use ic_identity::{get_identity_from_key_pair, load_key_pair};
use std::{
    fs::{self, File},
    path::Path,
    sync::mpsc::{self as std_mpsc, Sender as StdSender},
    time::{SystemTime, UNIX_EPOCH},
};
use structopt::StructOpt;
use tracing::{info, Dispatch};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{filter, prelude::*};
use tracing_timing::{Builder, Histogram, TimingLayer};

mod canister_methods;
mod canister_poller;
mod client_connection_handler;
mod gateway_server;
mod unit_tests;
mod ws_listener;

#[derive(Debug, StructOpt)]
#[structopt(name = "Gateway", about = "IC WS Gateway")]
struct DeploymentInfo {
    #[structopt(long, default_value = "http://127.0.0.1:4943")]
    subnet_url: String,

    #[structopt(long, default_value = "0.0.0.0:8080")]
    gateway_address: String,

    #[structopt(long, default_value = "100")]
    polling_interval: u64,

    #[structopt(long, default_value = "30000")]
    send_status_interval: u64,

    #[structopt(long)]
    tls_certificate_pem_path: Option<String>,

    #[structopt(long)]
    tls_certificate_key_pem_path: Option<String>,
}

#[derive(Debug)]
pub struct TimingData {
    event: String,
    min: u64,
    mean: f64,
    max: u64,
    count: u64,
}

fn create_data_dir() -> Result<(), String> {
    if !Path::new("./data").is_dir() {
        fs::create_dir("./data").map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn init_tracing() -> Result<(WorkerGuard, WorkerGuard, Dispatch), String> {
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
        .json()
        .with_writer(non_blocking_file)
        .with_thread_ids(true);
    let debug_log_stdout = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_stdout)
        .pretty();
    let timing_layer = Builder::default()
        .layer(|| Histogram::new_with_max(20_000_000_000, 1).unwrap())
        .with_filter(filter::filter_fn(|metadata| {
            metadata.level() == &filter::LevelFilter::TRACE
        }));

    let my_registry = tracing_subscriber::registry()
        .with(debug_log_file.with_filter(filter::LevelFilter::INFO))
        .with(debug_log_stdout.with_filter(filter::LevelFilter::INFO));

    let dispatch = Dispatch::new(timing_layer.with_subscriber(my_registry));
    dispatch.clone().init();

    Ok((guard_file, guard_stdout, dispatch))
}

fn start_time_traces_thread(dispatch: Dispatch, tracing_timing_tx: StdSender<TimingData>) {
    std::thread::spawn(move || loop {
        dispatch
            .downcast_ref::<TimingLayer>()
            .unwrap()
            .force_synchronize();
        dispatch
            .downcast_ref::<TimingLayer>()
            .unwrap()
            .with_histograms(|hs| {
                if let Some(hs) = &mut hs.get_mut("request") {
                    if let Some(h) = hs.get_mut("incoming_request") {
                        let count = h.len();
                        if count > 20 {
                            let data = TimingData {
                                event: String::from("incoming_request"),
                                min: h.min(),
                                mean: h.mean(),
                                max: h.max(),
                                count: count,
                            };
                            h.clear();
                            tracing_timing_tx.send(data).unwrap();
                        }
                    }
                    if let Some(h) = hs.get_mut("accepted_without_tls") {
                        let count = h.len();
                        if count > 20 {
                            let data = TimingData {
                                event: String::from("accepted_without_tls"),
                                min: h.min(),
                                mean: h.mean(),
                                max: h.max(),
                                count: count,
                            };
                            h.clear();
                            tracing_timing_tx.send(data).unwrap();
                        }
                    }
                }
            });
    });
}

#[tokio::main]
async fn main() -> Result<(), String> {
    create_data_dir()?;
    let (_guard_file, _guard_stdout, dispatch) = init_tracing().expect("could not init tracing");

    let deployment_info = DeploymentInfo::from_args();
    info!("Deployment info: {:?}", deployment_info);

    let key_pair = load_key_pair("./data/key_pair")?;
    let identity = get_identity_from_key_pair(key_pair);

    let mut gateway_server = GatewayServer::new(
        deployment_info.gateway_address,
        deployment_info.subnet_url,
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

    let (tracing_timing_tx, tracing_timing_rx) = std_mpsc::channel();
    start_time_traces_thread(dispatch, tracing_timing_tx);

    // maintains the WS Gateway state of the main task in sync with the spawned tasks
    gateway_server
        .manage_state(
            deployment_info.polling_interval,
            deployment_info.send_status_interval,
            tracing_timing_rx,
        )
        .await;
    info!("Terminated state manager");

    Ok(())
}

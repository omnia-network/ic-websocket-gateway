use gateway_server::GatewayServer;
use ic_agent::identity::BasicIdentity;
use tracing::info;
use tracing_subscriber::{filter, prelude::*};

use std::{
    fs::{self, File},
    path::Path,
    sync::Arc,
};
use structopt::StructOpt;

mod canister_methods;
mod canister_poller;
mod client_connection_handler;
mod gateway_server;
mod unit_tests;

#[derive(Debug, StructOpt)]
#[structopt(name = "Gateway", about = "IC WS Gateway")]
struct DeploymentInfo {
    #[structopt(short, long, default_value = "http://127.0.0.1:4943")]
    subnet_address: String,

    #[structopt(short, long, default_value = "0.0.0.0:8080")]
    gateway_address: String,

    #[structopt(short, long, default_value = "200")]
    polling_interval: u64,
}

fn load_key_pair() -> ring::signature::Ed25519KeyPair {
    if !Path::new("./data/key_pair").is_file() {
        let rng = ring::rand::SystemRandom::new();
        let key_pair = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
            .expect("Could not generate a key pair.");
        // TODO: print out seed phrase
        fs::write("./data/key_pair", key_pair.as_ref()).unwrap();
        ring::signature::Ed25519KeyPair::from_pkcs8(key_pair.as_ref())
            .expect("Could not read the key pair.")
    } else {
        let key_pair = fs::read("./data/key_pair").unwrap();
        ring::signature::Ed25519KeyPair::from_pkcs8(&key_pair)
            .expect("Could not read the key pair.")
    }
}

fn init_tracing() {
    if !Path::new("./data/traces").is_dir() {
        fs::create_dir("./data/traces").unwrap();
    }
    let file = File::create("./data/traces/gateway_debug.log").expect("could not create file");
    let debug_log = tracing_subscriber::fmt::layer().with_writer(Arc::new(file));
    tracing_subscriber::registry()
        .with(debug_log.with_filter(filter::LevelFilter::INFO))
        .init();
}

fn create_data_dir() {
    if !Path::new("./data").is_dir() {
        fs::create_dir("./data").unwrap();
    }
}

#[tokio::main]
async fn main() {
    create_data_dir();
    init_tracing();

    let deployment_info = DeploymentInfo::from_args();
    info!("Deployment info: {:?}", deployment_info);

    let key_pair = load_key_pair();
    let identity = BasicIdentity::from_key_pair(key_pair);

    let mut gateway_server = GatewayServer::new(
        &deployment_info.gateway_address,
        &deployment_info.subnet_address,
        identity,
    )
    .await;

    // spawn a task which keeps accepting incoming connection requests from WebSocket clients
    gateway_server.start_accepting_incoming_connections();

    // maintains the WS Gateway state of the main task in sync with the spawned tasks
    gateway_server
        .manage_state(deployment_info.polling_interval)
        .await;
}

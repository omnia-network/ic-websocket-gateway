use candid::Principal;
use opentelemetry_sdk::trace;
use std::{
    fs::{self, File},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{prelude::*, EnvFilter};

pub struct InitTracingResult {
    pub guards: (WorkerGuard, WorkerGuard),
    pub is_telemetry_enabled: bool,
}

pub fn init_tracing(
    telemetry_jaeger_agent_endpoint: Option<String>,
    gateway_principal: Principal,
) -> Result<InitTracingResult, String> {
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

    let env_filter_file = EnvFilter::builder()
        .with_env_var("RUST_LOG_FILE")
        .try_from_env()
        .unwrap_or_else(|_| EnvFilter::new("ic_websocket_gateway=trace"));

    let file_tracing_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(non_blocking_file)
        .with_thread_ids(true)
        .with_filter(env_filter_file);

    let env_filter_stdout = EnvFilter::builder()
        .with_env_var("RUST_LOG_STDOUT")
        .try_from_env()
        .unwrap_or_else(|_| EnvFilter::new("ic_websocket_gateway=info"));
    let stdout_tracing_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_stdout)
        .pretty()
        .with_filter(env_filter_stdout);

    let is_telemetry_enabled =
        match telemetry_jaeger_agent_endpoint
            .and_then(|s| if s.is_empty() { None } else { Some(s) })
        {
            Some(telemetry_jaeger_agent_endpoint) => {
                opentelemetry::global::set_text_map_propagator(
                    opentelemetry_jaeger::Propagator::new(),
                );

                let tracer = opentelemetry_jaeger::new_agent_pipeline()
                    .with_service_name(
                        "ic-ws-gw-".to_string() + &gateway_principal.to_string()[..5],
                    )
                    .with_max_packet_size(9216) // on MacOS 9216 is the max amount of bytes that can be sent in a single UDP packet
                    .with_endpoint(telemetry_jaeger_agent_endpoint)
                    .with_auto_split_batch(true)
                    .with_trace_config(
                        trace::config().with_sampler(trace::Sampler::TraceIdRatioBased(1.0)),
                    )
                    .install_batch(opentelemetry_sdk::runtime::Tokio)
                    .expect("should set up machinery to export data");
                let env_filter_telemetry = EnvFilter::builder()
                    .with_env_var("RUST_LOG_TELEMETRY")
                    .try_from_env()
                    .unwrap_or_else(|_| EnvFilter::new("ic_websocket_gateway=trace"));
                let opentelemetry = tracing_opentelemetry::layer()
                    .with_tracer(tracer)
                    .with_filter(env_filter_telemetry);

                let subscriber = tracing_subscriber::registry()
                    .with(file_tracing_layer)
                    .with(stdout_tracing_layer)
                    .with(opentelemetry);
                tracing::subscriber::set_global_default(subscriber).expect("should set subscriber");

                true
            },
            None => {
                let subscriber = tracing_subscriber::registry()
                    .with(file_tracing_layer)
                    .with(stdout_tracing_layer);
                tracing::subscriber::set_global_default(subscriber).expect("should set subscriber");

                false
            },
        };

    println!("Tracing telemetry enabled: {}", is_telemetry_enabled);

    Ok(InitTracingResult {
        guards: (guard_file, guard_stdout),
        is_telemetry_enabled,
    })
}

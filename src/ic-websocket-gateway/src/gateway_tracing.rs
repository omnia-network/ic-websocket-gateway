use candid::Principal;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use std::{
    fs::{self, File},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{prelude::*, EnvFilter};

pub struct InitTracingResult {
    pub guards: (WorkerGuard, WorkerGuard),
    pub is_telemetry_enabled: bool,
}

pub fn init_tracing(
    opentelemetry_collector_endpoint: Option<String>,
    gateway_principal: Principal,
) -> Result<InitTracingResult, String> {
    // stdout tracing
    let (stdout_tracing_layer, guard_stdout) = {
        let (non_blocking_stdout, guard_stdout) = tracing_appender::non_blocking(std::io::stdout());

        let env_filter_stdout = EnvFilter::builder()
            .with_env_var("RUST_LOG_STDOUT")
            .try_from_env()
            .unwrap_or_else(|_| EnvFilter::new("ic_websocket_gateway=info"));
        let stdout_tracing_layer = tracing_subscriber::fmt::layer()
            .with_writer(non_blocking_stdout)
            .pretty()
            .with_filter(env_filter_stdout);

        (stdout_tracing_layer, guard_stdout)
    };

    // file tracing
    let (file_tracing_layer, guard_file) = {
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

        let env_filter_file = EnvFilter::builder()
            .with_env_var("RUST_LOG_FILE")
            .try_from_env()
            .unwrap_or_else(|_| {
                // disable file tracing by default
                EnvFilter::builder()
                    .with_default_directive(LevelFilter::OFF.into())
                    .parse("")
                    .expect("failed to parse default filter for file tracing")
            });

        let file_tracing_layer = tracing_subscriber::fmt::layer()
            .json()
            .with_writer(non_blocking_file)
            .with_thread_ids(true)
            .with_filter(env_filter_file);

        (file_tracing_layer, guard_file)
    };

    let is_telemetry_enabled =
        match opentelemetry_collector_endpoint
            .and_then(|s| if s.is_empty() { None } else { Some(s) })
        {
            Some(opentelemetry_collector_endpoint) => {
                let otlp_exporter = opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(opentelemetry_collector_endpoint)
                    .with_protocol(Protocol::Grpc);

                let otlp_config =
                    opentelemetry_sdk::trace::config().with_resource(Resource::new(vec![
                        KeyValue::new(
                            "service.name",
                            "ic-ws-gw-".to_string() + &gateway_principal.to_string()[..5],
                        ),
                    ]));

                let otlp_tracer = opentelemetry_otlp::new_pipeline()
                    .tracing()
                    .with_exporter(otlp_exporter)
                    .with_trace_config(otlp_config)
                    .install_batch(opentelemetry_sdk::runtime::Tokio)
                    .expect("failed to install");

                let env_filter_telemetry = EnvFilter::builder()
                    .with_env_var("RUST_LOG_TELEMETRY")
                    .try_from_env()
                    .unwrap_or_else(|_| EnvFilter::new("ic_websocket_gateway=trace"));

                let opentelemetry = tracing_opentelemetry::layer()
                    .with_tracer(otlp_tracer)
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

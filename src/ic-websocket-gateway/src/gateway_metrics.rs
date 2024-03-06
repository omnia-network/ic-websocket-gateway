use std::error::Error;
use std::time::Duration;
use metrics::{describe_gauge, describe_histogram, gauge};
use metrics_exporter_prometheus::PrometheusBuilder;
use metrics_util::{MetricKindMask};
// use opentelemetry::{global, KeyValue};
// use opentelemetry_otlp::{Protocol, WithExportConfig};

pub fn init_metrics() -> Result<(), Box<dyn Error>> {
    let builder = PrometheusBuilder::new();
    builder
        .idle_timeout(
            MetricKindMask::COUNTER | MetricKindMask::HISTOGRAM,
            Some(Duration::from_secs(10)),
        )
        .install()
        .expect("failed to install Prometheus recorder");

    describe_gauge!("clients_connected", "The number of clients currently connected");
    describe_histogram!(
        "connection_duration",
        "The duration of the client connection"
    );

    gauge!("clients_connected").set(0.0);

    Ok(())
}

// pub fn init_metrics(opentelemetry_collector_endpoint: Option<String>) -> Result<(), String> {
//     match opentelemetry_collector_endpoint
//         .and_then(|s| if s.is_empty() { None } else { Some(s) })
//     {
//         Some(opentelemetry_collector_endpoint) => {
//             let exporter = opentelemetry_otlp::new_exporter()
//                 .tonic()
//                 .with_endpoint(opentelemetry_collector_endpoint)
//                 .with_protocol(Protocol::Grpc);
//
//             let meter_provider = opentelemetry_otlp::new_pipeline()
//                 .metrics(tokio::spawn, tokio::time::sleep)
//                 .with_exporter(exporter)
//                 .build()
//                 .unwrap();
//
//             global::set_meter_provider(meter_provider);
//
//             let meter = global::meter("my_app");
//             let counter = meter.u64_counter("my_counter").init();
//             counter.add(1, &[KeyValue::new("key", "value")]);
//
//             // Important: Give some time for metrics to be exported
//             tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
//         }
//         None => {}
//     }
//
//     register_custom_metrics();
//
//     Ok(())
// }

use metrics::{describe_counter, describe_gauge, describe_histogram, gauge};
use metrics_exporter_prometheus::PrometheusBuilder;
use metrics_util::MetricKindMask;
use std::error::Error;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

pub fn init_metrics(address: &str) -> Result<(), Box<dyn Error>> {
    let builder = PrometheusBuilder::new().with_http_listener(SocketAddr::from_str(address)?);

    // Set the idle timeout for counters and histograms to 10 seconds then the metrics are removed from the registry
    builder
        .idle_timeout(MetricKindMask::ALL, Some(Duration::from_secs(10)))
        .install()
        .expect("failed to install Prometheus recorder");

    describe_counter!("client_connected_count", "Each time that a client connects it emits a point");
    describe_gauge!(
        "clients_connected",
        "The number of clients currently connected"
    );
    describe_gauge!("active_pollers", "The number of pollers currently active");
    describe_histogram!(
        "connection_duration",
        "The duration of the client connection"
    );
    describe_histogram!(
        "tls_resolution_time",
        "The resolution time of the TLS handshake"
    );
    describe_histogram!(
        "connection_opening_time",
        "The time it takes to open a connection"
    );
    describe_histogram!(
        "poller_duration",
        "The time it takes to poll the canister"
    );

    gauge!("active_pollers").set(0.0);

    Ok(())
}

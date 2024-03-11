use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use metrics::{describe_gauge, describe_histogram, gauge};
use metrics_exporter_prometheus::PrometheusBuilder;
use metrics_util::{MetricKindMask};

pub fn init_metrics(port: Option<u16>) -> Result<(), Box<dyn Error>> {
    let builder = PrometheusBuilder::new()
        .with_http_listener(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port.unwrap_or(9000)));
    builder
        .idle_timeout(
            MetricKindMask::ALL,
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

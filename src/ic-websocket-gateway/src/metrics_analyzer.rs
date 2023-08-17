use std::time::Duration;
use tokio::{sync::mpsc::Receiver, time::Instant};
use tracing::info;

/// trait implemented by the structs containing the relevant events of each component
pub trait Metrics {
    /// returns the value used to compute the time interval between two metrics
    fn get_value_for_interval(&self) -> TimeableEvent;

    /// returns the time deltas between the evets in the current metric and the time interval from the previous one
    fn compute_deltas(&self, previous: Box<dyn Metrics + Send>) -> Option<Box<dyn Deltas>>;
}

/// trait implemented by the structs containing the analytics of each component
pub trait Deltas {
    /// displays all the deltas of a metric
    fn display(&self);

    /// returns the time interval between two metrics
    fn get_interval(&self) -> Duration;
}

#[derive(Debug, Clone)]
/// struct containing the instant of an event and helper methods to calculate duration between events
pub struct TimeableEvent {
    instant: Option<Instant>,
}

impl TimeableEvent {
    pub fn default() -> Self {
        Self { instant: None }
    }

    pub fn now() -> Self {
        Self {
            instant: Some(Instant::now()),
        }
    }

    /// sets to current instant
    pub fn set_now(&mut self) {
        self.instant = Some(Instant::now());
    }

    /// measures the time between two events
    pub fn duration_since(&self, other: &TimeableEvent) -> Option<Duration> {
        Some(self.instant?.duration_since(other.instant?))
    }
}

/// metrics analyzer receives metrics from different components of the WS Gateway
pub struct MetricsAnalyzer {
    /// receiver of the channel used to send metrics to the analyzer
    metrics_channel_rx: Receiver<Box<dyn Metrics + Send>>,
}

impl MetricsAnalyzer {
    pub fn new(metrics_channel_rx: Receiver<Box<dyn Metrics + Send>>) -> Self {
        Self { metrics_channel_rx }
    }

    // process the received metrics
    pub async fn start_processing(&mut self) {
        let mut aggregated_intervals = Vec::new();
        let mut previous = None;
        let mut iter = 0;
        loop {
            if let Some(metrics) = self.metrics_channel_rx.recv().await {
                // skip the first metric as it does not have a previous one
                if let Some(previous) = previous {
                    if let Some(deltas) = metrics.compute_deltas(previous) {
                        aggregated_intervals.push(deltas.get_interval());
                        deltas.display();
                    }
                }
                previous = Some(metrics);
                iter += 1;
                if iter > 10 {
                    let sum: Duration = aggregated_intervals.iter().sum();
                    let avg = sum.div_f64(aggregated_intervals.len() as f64);
                    info!("Average interval: {:?}", avg);
                    aggregated_intervals = Vec::new();
                    iter = 0;
                }
            }
        }
    }
}

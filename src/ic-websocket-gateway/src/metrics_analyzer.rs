use std::any::type_name;
use std::fmt::Debug;
use std::{collections::BTreeMap, time::Duration};
use tokio::{sync::mpsc::Receiver, time::Instant};
use tracing::{debug, info};

/// trait implemented by the structs containing the relevant events of each component
pub trait Metrics: Debug {
    fn get_type_name(&self) -> String {
        let path: Vec<String> = type_name::<Self>()
            .split("::")
            .map(|s| s.to_string())
            .collect();
        path.last().expect("not a valid path").to_owned()
    }

    /// returns the value used to compute the time interval between two metrics
    fn get_value_for_interval(&self) -> &TimeableEvent;

    /// returns the time deltas between the evets in the current metric and the time interval from the previous one
    fn compute_deltas(&self, previous: &Box<dyn Metrics + Send>) -> Option<Box<dyn Deltas + Send>>;

    fn compute_latency(&self) -> Option<Duration>;
}

/// trait implemented by the structs containing the analytics of each component
pub trait Deltas: Debug {
    /// displays all the deltas of a metric
    fn display(&self);

    /// returns the time interval between two metrics
    fn get_interval(&self) -> Duration;

    /// returns the latency of the component
    fn get_latency(&self) -> Duration;
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

    pub fn is_set(&self) -> bool {
        self.instant.is_some()
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
    aggregated_deltas_map: BTreeMap<String, (Vec<Box<dyn Deltas + Send>>, Box<dyn Metrics + Send>)>,
}

impl MetricsAnalyzer {
    pub fn new(metrics_channel_rx: Receiver<Box<dyn Metrics + Send>>) -> Self {
        let aggregated_deltas_map = BTreeMap::default();
        Self {
            metrics_channel_rx,
            aggregated_deltas_map,
        }
    }

    // process the received metrics
    pub async fn start_processing(&mut self) {
        loop {
            if let Some(metrics) = self.metrics_channel_rx.recv().await {
                let metric_type_name = metrics.get_type_name();
                if let Some((aggregated_deltas, previous)) =
                    self.aggregated_deltas_map.get_mut(&metric_type_name)
                {
                    if let Some(deltas) = metrics.compute_deltas(previous) {
                        deltas.display();
                        aggregated_deltas.push(deltas);
                    }
                    *previous = metrics;
                    if aggregated_deltas.len() > 10 {
                        let (intervals, latencies) = aggregated_deltas.iter().fold(
                            (Vec::new(), Vec::new()),
                            |(mut intervals, mut latencies), deltas| {
                                intervals.push(deltas.get_interval());
                                latencies.push(deltas.get_latency());
                                (intervals, latencies)
                            },
                        );
                        let sum_intervals: Duration = intervals.iter().sum();
                        let avg_interval = sum_intervals.div_f64(aggregated_deltas.len() as f64);
                        debug!(
                            "Average interval for {:?}: {:?}",
                            metric_type_name, avg_interval
                        );
                        let sum_latencies: Duration = latencies.iter().sum();
                        let avg_latency = sum_latencies.div_f64(aggregated_deltas.len() as f64);
                        info!(
                            "Average latency for {:?}: {:?}",
                            metric_type_name, avg_latency
                        );
                        *aggregated_deltas = Vec::new();
                    }
                } else {
                    self.aggregated_deltas_map
                        .insert(metric_type_name, (Vec::new(), metrics));
                }
            }
        }
    }
}

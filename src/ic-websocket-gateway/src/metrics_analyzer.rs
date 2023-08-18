use std::any::{type_name, Any};
use std::cmp::Ordering;
use std::fmt::Debug;
use std::{collections::BTreeMap, time::Duration};
use tokio::{sync::mpsc::Receiver, time::Instant};
use tracing::info;

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
    fn compute_deltas(&self) -> Option<Box<dyn Deltas + Send>>;

    fn compute_interval(&self, previous: &Box<dyn Metrics + Send>) -> Duration {
        self.get_value_for_interval()
            .duration_since(previous.get_value_for_interval())
            .expect("previous metric must exist")
    }

    fn compute_latency(&self) -> Option<Duration>;
}

/// trait implemented by the structs containing the deltas computed within each component
pub trait Deltas: Debug {
    /// displays all the deltas of a metric
    fn display(&self);

    /// returns the latency of the component
    fn get_latency(&self) -> Duration;

    fn get_reference(&self) -> &MetricsReference;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricsReference {
    MessageNonce(u64),
    ClientId(u64),
    Timestamp(u64),
}

impl MetricsReference {
    fn get_inner_value(&self) -> Option<&dyn Any> {
        match self {
            MetricsReference::MessageNonce(nonce) => Some(nonce as &dyn Any),
            MetricsReference::ClientId(id) => Some(id as &dyn Any),
            MetricsReference::Timestamp(id) => Some(id as &dyn Any),
        }
    }
}

impl Ord for MetricsReference {
    fn cmp(&self, other: &Self) -> Ordering {
        self.get_inner_value()
            .expect("does not have inner value")
            .downcast_ref::<u64>()
            .cmp(
                &other
                    .get_inner_value()
                    .expect("does not have inner value")
                    .downcast_ref::<u64>(),
            )
    }
}

// Implement the PartialEq trait as well (required for Ord implementation)
impl PartialOrd for MetricsReference {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct AggregatedMetrics {
    deltas: Box<dyn Deltas + Send>,
    interval: Duration,
}

impl AggregatedMetrics {
    fn new(deltas: Box<dyn Deltas + Send>, interval: Duration) -> Self {
        Self { deltas, interval }
    }
}

/// metrics analyzer receives metrics from different components of the WS Gateway
pub struct MetricsAnalyzer {
    /// receiver of the channel used to send metrics to the analyzer
    metrics_channel_rx: Receiver<Box<dyn Metrics + Send>>,
    aggregated_data_map: BTreeMap<
        String,
        (
            BTreeMap<MetricsReference, AggregatedMetrics>,
            Box<dyn Metrics + Send>,
        ),
    >,
}

impl MetricsAnalyzer {
    pub fn new(metrics_channel_rx: Receiver<Box<dyn Metrics + Send>>) -> Self {
        let aggregated_data_map = BTreeMap::default();
        Self {
            metrics_channel_rx,
            aggregated_data_map,
        }
    }

    // process the received metrics
    pub async fn start_processing(&mut self) {
        loop {
            if let Some(metrics) = self.metrics_channel_rx.recv().await {
                let metric_type_name = metrics.get_type_name();
                // first metrics received does not result in a delta as there is no previous metric for computing interval
                if let Some((aggregated_data, previous)) =
                    self.aggregated_data_map.get_mut(&metric_type_name)
                {
                    if let Some(deltas) = metrics.compute_deltas() {
                        deltas.display();
                        let reference = deltas.get_reference().clone();
                        let aggregated_metrics =
                            AggregatedMetrics::new(deltas, metrics.compute_interval(previous));
                        aggregated_data.insert(reference, aggregated_metrics);
                    }
                    *previous = metrics;
                    if aggregated_data.len() > 10 {
                        let (latencies, intervals) = aggregated_data.iter().fold(
                            (Vec::new(), Vec::new()),
                            |(mut latencies, mut intervals), (_, aggregated_metrics)| {
                                latencies.push(aggregated_metrics.deltas.get_latency());
                                intervals.push(aggregated_metrics.interval);
                                (latencies, intervals)
                            },
                        );
                        let sum_latencies: Duration = latencies.iter().sum();
                        let avg_latency = sum_latencies.div_f64(latencies.len() as f64);
                        info!(
                            "Average latency for {:?}: {:?}",
                            metric_type_name, avg_latency
                        );
                        let sum_intervals: Duration = intervals.iter().sum();
                        let avg_interval = sum_intervals.div_f64(intervals.len() as f64);
                        info!(
                            "Average interval for {:?}: {:?}",
                            metric_type_name, avg_interval
                        );
                        *aggregated_data = BTreeMap::default();
                    }
                } else {
                    self.aggregated_data_map
                        .insert(metric_type_name, (BTreeMap::default(), metrics));
                }
            }
        }
    }
}

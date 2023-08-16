use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use tracing::info;

/// trait implemented by the structs containing the relevant events of each component
pub trait Metrics<S = Self> {
    // bound ReturnType to implementors of the Deltas trait
    type ReturnType: Deltas;
    type Param;

    fn compute_deltas(&self, previous: S) -> Option<Self::ReturnType>;
}

/// trait implemented by the structs containing the analytics of each component
pub trait Deltas {
    fn display(&self);

    fn get_interval(&self) -> Duration;
}

/// metrics analyzer receives metrics from different components of the WS Gateway
pub struct MetricsAnalyzer<T: Metrics> {
    /// receiver of the channel used to send metrics to the analyzer
    metrics_channel_rx: Receiver<T>,
}

impl<T: Metrics> MetricsAnalyzer<T> {
    pub fn new(metrics_channel_rx: Receiver<T>) -> Self {
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

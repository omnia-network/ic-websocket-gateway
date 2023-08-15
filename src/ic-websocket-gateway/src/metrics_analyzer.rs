use std::time::Duration;

use tokio::sync::mpsc::Receiver;
use tracing::info;

pub trait Metrics<S = Self> {
    type ReturnType: Deltas;
    type Param;

    fn get_interval(&self, previous: S) -> Option<Duration>;

    fn compute_deltas(&self) -> Option<Self::ReturnType>;
}

pub trait Deltas {
    fn display(&self);
}
pub struct MetricsAnalyzer<T: Metrics> {
    metrics_channel_rx: Receiver<T>,
}

impl<T: Metrics + Clone> MetricsAnalyzer<T> {
    pub fn new(metrics_channel_rx: Receiver<T>) -> Self {
        Self { metrics_channel_rx }
    }

    pub async fn start_processing(&mut self) {
        let mut previous = None;
        loop {
            if let Some(metrics) = self.metrics_channel_rx.recv().await {
                if let Some(previous) = previous {
                    let interval = metrics.get_interval(previous);
                    info!("Effective polling interval: {:?}", interval);
                }
                previous = Some(metrics.clone());
                if let Some(deltas) = metrics.compute_deltas() {
                    deltas.display();
                }
            }
        }
    }
}

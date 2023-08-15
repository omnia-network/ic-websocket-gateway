use tokio::sync::mpsc::Receiver;

use crate::canister_poller::PollerMetrics;

pub struct MetricsAnalyzer {
    metrics_channel_rx: Receiver<PollerMetrics>,
}

impl MetricsAnalyzer {
    pub fn new(metrics_channel_rx: Receiver<PollerMetrics>) -> Self {
        Self { metrics_channel_rx }
    }

    pub async fn start_processing(&mut self) {
        loop {
            if let Some(poller_metrics) = self.metrics_channel_rx.recv().await {
                if let Some(deltas) = poller_metrics.compute_deltas() {
                    deltas.display();
                }
            }
        }
    }
}

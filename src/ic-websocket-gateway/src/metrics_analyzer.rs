use tokio::sync::mpsc::Receiver;

pub trait Metrics {
    type Item: Deltas;

    fn compute_deltas(&self) -> Option<Self::Item>;
}

pub trait Deltas {
    fn display(&self);
}
pub struct MetricsAnalyzer<T: Metrics> {
    metrics_channel_rx: Receiver<T>,
}

impl<T: Metrics> MetricsAnalyzer<T> {
    pub fn new(metrics_channel_rx: Receiver<T>) -> Self {
        Self { metrics_channel_rx }
    }

    pub async fn start_processing(&mut self) {
        loop {
            if let Some(metrics) = self.metrics_channel_rx.recv().await {
                if let Some(deltas) = metrics.compute_deltas() {
                    deltas.display();
                }
            }
        }
    }
}

use tokio::sync::mpsc::Receiver;

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
        let mut previous = None;
        loop {
            if let Some(metrics) = self.metrics_channel_rx.recv().await {
                // skip the first metric as it does not have a previous one
                if let Some(previous) = previous {
                    if let Some(deltas) = metrics.compute_deltas(previous) {
                        deltas.display();
                    }
                }
                previous = Some(metrics);
            }
        }
    }
}

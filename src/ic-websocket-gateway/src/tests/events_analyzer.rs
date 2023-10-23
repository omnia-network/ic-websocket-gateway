#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{
        sync::mpsc::{self, Receiver, Sender},
        time::sleep,
    };

    use crate::{
        events_analyzer::{Events, EventsAnalyzer, EventsCollectionType, EventsReference},
        metrics::ws_listener_metrics::{ListenerEvents, ListenerEventsMetrics},
    };

    async fn get_listener_events_with_one_ms_latency(client_id: u64) -> Box<dyn Events + Send> {
        let mut listener_events = ListenerEvents::new(
            Some(EventsReference::ClientId(client_id)),
            EventsCollectionType::NewClientConnection,
            ListenerEventsMetrics::default(),
        );
        listener_events.metrics.set_received_request();
        listener_events.metrics.set_accepted_without_tls();
        sleep(Duration::from_millis(1)).await;
        listener_events.metrics.set_started_handler();
        Box::new(listener_events)
    }
    #[tokio::test()]
    async fn should_record_first_latency_but_not_interval() {
        let (_events_channel_tx, events_channel_rx) = mpsc::channel(100);

        let (rate_limiting_channel_tx, _rate_limiting_channel_rx): (
            Sender<Option<f64>>,
            Receiver<Option<f64>>,
        ) = mpsc::channel(10);

        let mut events_analyzer =
            EventsAnalyzer::new(events_channel_rx, rate_limiting_channel_tx, 100, 1);

        let client_id = 0;
        let events = get_listener_events_with_one_ms_latency(client_id).await;
        let reference = events.get_reference();
        if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
            events_analyzer.add_latency_to_collection(&events, &deltas);
            events_analyzer.add_interval_to_events(events);
        }

        // first latency should be recorded
        assert_eq!(events_analyzer.map_latencies_by_collection_type.len(), 1);
        assert_eq!(
            events_analyzer
                .map_latencies_by_collection_type
                .get(&EventsCollectionType::NewClientConnection)
                .unwrap()
                .get(&EventsReference::ClientId(client_id))
                .unwrap()
                .get_inner()
                .len(),
            1
        );
        assert_eq!(events_analyzer.aggregated_latencies_map.len(), 0);

        // first interval should not be recorded
        // however the event should be stored so that we can compute the interval once we receive the next event of the same type
        assert_eq!(events_analyzer.map_intervals_by_events_type.len(), 1);
        assert_eq!(
            events_analyzer
                .map_intervals_by_events_type
                .get("ListenerEventsMetrics")
                .unwrap()
                .intervals
                .len(),
            0
        )
    }
}

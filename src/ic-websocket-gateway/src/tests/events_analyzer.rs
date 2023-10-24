#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{
        sync::mpsc::{self, Receiver, Sender},
        time::sleep,
    };

    use crate::{
        events_analyzer::{Events, EventsAnalyzer, EventsCollectionType, EventsReference},
        metrics::{
            client_connection_handler_metrics::{
                RequestConnectionSetupEvents, RequestConnectionSetupEventsMetrics,
            },
            gateway_server_metrics::{
                ConnectionEstablishmentEvents, ConnectionEstablishmentEventsMetrics,
            },
            ws_listener_metrics::{ListenerEvents, ListenerEventsMetrics},
        },
    };

    fn init_events_analyzer(compute_average_threshold: u64) -> EventsAnalyzer {
        let (_events_channel_tx, events_channel_rx) = mpsc::channel(100);

        let (rate_limiting_channel_tx, _rate_limiting_channel_rx): (
            Sender<Option<f64>>,
            Receiver<Option<f64>>,
        ) = mpsc::channel(10);

        EventsAnalyzer::new(
            events_channel_rx,
            rate_limiting_channel_tx,
            100,
            compute_average_threshold,
        )
    }

    async fn get_listener_events_with_one_ms_latency(
        client_id: u64,
        latency_ms: u64,
    ) -> Box<dyn Events + Send> {
        let mut listener_events = ListenerEvents::new(
            Some(EventsReference::ClientId(client_id)),
            EventsCollectionType::NewClientConnection,
            ListenerEventsMetrics::default(),
        );
        listener_events.metrics.set_received_request();
        listener_events.metrics.set_accepted_without_tls();
        sleep(Duration::from_millis(latency_ms)).await;
        listener_events.metrics.set_started_handler();
        Box::new(listener_events)
    }

    async fn get_request_connection_setup_events_with_one_ms_latency(
        client_id: u64,
        latency_ms: u64,
    ) -> Box<dyn Events + Send> {
        let mut request_connection_setup_events = RequestConnectionSetupEvents::new(
            Some(EventsReference::ClientId(client_id)),
            EventsCollectionType::NewClientConnection,
            RequestConnectionSetupEventsMetrics::default(),
        );
        request_connection_setup_events
            .metrics
            .set_accepted_ws_connection();
        sleep(Duration::from_millis(latency_ms)).await;
        request_connection_setup_events
            .metrics
            .set_ws_connection_setup();
        Box::new(request_connection_setup_events)
    }

    async fn get_connection_establishment_events_with_on_ms_latency(
        client_id: u64,
        latency_ms: u64,
    ) -> Box<dyn Events + Send> {
        let mut connection_establishment_events = ConnectionEstablishmentEvents::new(
            Some(EventsReference::ClientId(client_id)),
            EventsCollectionType::NewClientConnection,
            ConnectionEstablishmentEventsMetrics::default(),
        );
        connection_establishment_events
            .metrics
            .set_added_client_to_state();
        connection_establishment_events
            .metrics
            .set_started_new_poller();
        sleep(Duration::from_millis(latency_ms)).await;
        connection_establishment_events
            .metrics
            .set_sent_client_channel_to_poller();
        Box::new(connection_establishment_events)
    }

    #[tokio::test()]
    async fn should_record_first_latency_but_not_interval() {
        let compute_average_threshold = 1;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let client_id = 0;
        let latency_ms = 10;
        let events = get_listener_events_with_one_ms_latency(client_id, latency_ms).await;
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

        // first interval should not be recorded (as there is no previous event of the same type)
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

    #[tokio::test()]
    async fn should_compute_average_latency() {
        let compute_average_threshold = 1;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let client_id = 0;
        let latency_ms = 10;
        let collection = vec![
            get_listener_events_with_one_ms_latency(client_id, latency_ms).await,
            get_request_connection_setup_events_with_one_ms_latency(client_id, latency_ms).await,
            get_connection_establishment_events_with_on_ms_latency(client_id, latency_ms).await,
        ];
        let events_count = collection.len() as u64;
        for events in collection {
            let reference = events.get_reference();
            if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                events_analyzer.add_latency_to_collection(&events, &deltas);
            }
        }

        let latencies = events_analyzer.compute_collections_latencies();
        let overhead_latency = 7;
        assert_eq!(latencies.len(), 1);
        assert_eq!(latencies[0].avg_type, "NewClientConnection");
        assert!(
            latencies[0].avg_value >= Duration::from_millis(events_count * latency_ms)
                && latencies[0].avg_value
                    <= Duration::from_millis(events_count * latency_ms + overhead_latency)
        );
        assert_eq!(latencies[0].count, 1);
    }
}

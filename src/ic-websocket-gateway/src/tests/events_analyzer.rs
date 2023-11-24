#[cfg(test)]
mod tests {
    use std::time::Duration;

    use candid::Principal;
    use tokio::{
        sync::mpsc::{self, Receiver, Sender},
        time::sleep,
    };

    use crate::{
        events_analyzer::{
            Events, EventsAnalyzer, EventsCollectionType, EventsReference, IterationReference,
            MessageReference,
        },
        metrics::{
            canister_poller_metrics::{
                IncomingCanisterMessageEvents, IncomingCanisterMessageEventsMetrics, PollerEvents,
                PollerEventsMetrics,
            },
            client_connection_handler_metrics::{
                OutgoingCanisterMessageEvents, OutgoingCanisterMessageEventsMetrics,
                RequestConnectionSetupEvents, RequestConnectionSetupEventsMetrics,
            },
            manager_metrics::{
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

    async fn get_listener_events_with_latency(
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

    async fn get_request_connection_setup_events_with_latency(
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

    async fn get_connection_establishment_events_with_latency(
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
            .set_received_client_session();
        connection_establishment_events
            .metrics
            .set_started_new_poller();
        sleep(Duration::from_millis(latency_ms)).await;
        connection_establishment_events
            .metrics
            .set_sent_client_channel_to_poller();
        Box::new(connection_establishment_events)
    }

    async fn get_outgoing_canister_message_events_with_latency(
        canister_id: Principal,
        message_nonce: u64,
        latency_ms: u64,
    ) -> Box<dyn Events + Send> {
        let message_key = MessageReference::new(canister_id, message_nonce);
        let mut outgoing_canister_message_events = OutgoingCanisterMessageEvents::new(
            Some(EventsReference::MessageReference(message_key)),
            EventsCollectionType::CanisterMessage,
            OutgoingCanisterMessageEventsMetrics::default(),
        );
        outgoing_canister_message_events
            .metrics
            .set_received_canister_message();
        sleep(Duration::from_millis(latency_ms)).await;
        outgoing_canister_message_events
            .metrics
            .set_message_sent_to_client();
        Box::new(outgoing_canister_message_events)
    }

    async fn get_incoming_canister_message_events_with_latency(
        canister_id: Principal,
        message_nonce: u64,
        latency_ms: u64,
    ) -> Box<dyn Events + Send> {
        let message_key = MessageReference::new(canister_id, message_nonce);
        let mut incoming_canister_message_events = IncomingCanisterMessageEvents::new(
            Some(EventsReference::MessageReference(message_key)),
            EventsCollectionType::CanisterMessage,
            IncomingCanisterMessageEventsMetrics::default(),
        );
        incoming_canister_message_events
            .metrics
            .set_start_relaying_message();
        sleep(Duration::from_millis(latency_ms)).await;
        incoming_canister_message_events
            .metrics
            .set_message_relayed();
        Box::new(incoming_canister_message_events)
    }

    async fn get_poller_events_with_latency(
        canister_id: Principal,
        polling_iteration: u64,
        latency_ms: u64,
    ) -> Box<dyn Events + Send> {
        let iteration_key = IterationReference::new(canister_id, polling_iteration);
        let mut poller_events = PollerEvents::new(
            Some(EventsReference::IterationReference(iteration_key)),
            EventsCollectionType::PollerStatus,
            PollerEventsMetrics::default(),
        );
        poller_events.metrics.set_start_polling();
        poller_events.metrics.set_received_messages();
        sleep(Duration::from_millis(latency_ms)).await;
        poller_events.metrics.set_start_relaying_messages();
        poller_events.metrics.set_finished_relaying_messages();
        Box::new(poller_events)
    }

    #[tokio::test()]
    async fn should_record_first_latency_but_not_interval() {
        let compute_average_threshold = 1;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let client_id = 0;
        let latency_ms = 10;
        let events = get_listener_events_with_latency(client_id, latency_ms).await;
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
            get_listener_events_with_latency(client_id, latency_ms).await,
            get_request_connection_setup_events_with_latency(client_id, latency_ms).await,
            get_connection_establishment_events_with_latency(client_id, latency_ms).await,
        ];
        let events_count = collection.len() as u64;
        for events in collection {
            let reference = events.get_reference();
            if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                events_analyzer.add_latency_to_collection(&events, &deltas);
            }
        }

        let latencies = events_analyzer.compute_collections_latencies();
        // if the test fails try to slightly increase 'latency_overhead'
        let latency_overhead = 7;
        assert_eq!(latencies.len(), 1);
        assert_eq!(latencies[0].avg_type, "NewClientConnection");
        assert!(
            latencies[0].value >= Duration::from_millis(events_count * latency_ms)
                && latencies[0].value
                    <= Duration::from_millis(events_count * latency_ms + latency_overhead)
        );
        assert_eq!(latencies[0].count, 1);
    }

    #[tokio::test()]
    async fn should_compute_average_interval() {
        let compute_average_threshold = 1;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let interval_ms = 1000;
        let events = get_listener_events_with_latency(0, 0).await;
        events_analyzer.add_interval_to_events(events);
        sleep(Duration::from_millis(interval_ms)).await;
        let events = get_listener_events_with_latency(1, 0).await;
        events_analyzer.add_interval_to_events(events);

        let intervals = events_analyzer.compute_average_intervals();
        let interval_overhead = 10;
        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0].avg_type, "ListenerEventsMetrics");
        assert!(
            intervals[0].value >= Duration::from_millis(interval_ms)
                && intervals[0].value <= Duration::from_millis(interval_ms + interval_overhead)
        );
        assert_eq!(intervals[0].count, 1);
    }

    #[tokio::test()]
    async fn should_not_compute_average_latency_as_incomplete_collection() {
        let compute_average_threshold = 1;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let client_id = 0;
        let latency_ms = 10;
        let collection = vec![
            get_listener_events_with_latency(client_id, latency_ms).await,
            // missing RequestConnectionSetupEventsMetrics
            get_connection_establishment_events_with_latency(client_id, latency_ms).await,
        ];
        for events in collection {
            let reference = events.get_reference();
            if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                events_analyzer.add_latency_to_collection(&events, &deltas);
            }
        }

        let latencies = events_analyzer.compute_collections_latencies();
        assert_eq!(latencies.len(), 0);
    }

    #[tokio::test()]
    async fn should_compute_average_latency_as_enough_collections() {
        let compute_average_threshold = 5;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let latency_ms = 10;
        for client_id in 0..compute_average_threshold {
            let collection = vec![
                get_listener_events_with_latency(client_id, latency_ms).await,
                get_request_connection_setup_events_with_latency(client_id, latency_ms).await,
                get_connection_establishment_events_with_latency(client_id, latency_ms).await,
            ];
            for events in collection {
                let reference = events.get_reference();
                if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                    events_analyzer.add_latency_to_collection(&events, &deltas);
                }
            }
        }

        let events_in_collection = 3;
        let latencies = events_analyzer.compute_collections_latencies();
        // if the test fails try to slightly increase 'latency_overhead'
        let latency_overhead = 7;
        assert_eq!(latencies.len(), 1);
        assert_eq!(latencies[0].avg_type, "NewClientConnection");
        assert!(
            latencies[0].value >= Duration::from_millis(events_in_collection * latency_ms)
                && latencies[0].value
                    <= Duration::from_millis(events_in_collection * latency_ms + latency_overhead)
        );
        assert_eq!(latencies[0].count, compute_average_threshold as usize);
    }

    #[tokio::test()]
    async fn should_not_compute_average_latency_as_not_enough_collections() {
        let compute_average_threshold = 5;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let latency_ms = 10;
        for client_id in 0..compute_average_threshold - 1 {
            let collection = vec![
                get_listener_events_with_latency(client_id, latency_ms).await,
                get_request_connection_setup_events_with_latency(client_id, latency_ms).await,
                get_connection_establishment_events_with_latency(client_id, latency_ms).await,
            ];
            for events in collection {
                let reference = events.get_reference();
                if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                    events_analyzer.add_latency_to_collection(&events, &deltas);
                }
            }
        }

        let latencies = events_analyzer.compute_collections_latencies();
        assert_eq!(latencies.len(), 0);
    }

    #[tokio::test()]
    async fn should_not_compute_average_latency_twice() {
        let compute_average_threshold = 5;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let latency_ms = 10;
        for client_id in 0..compute_average_threshold {
            let collection = vec![
                get_listener_events_with_latency(client_id, latency_ms).await,
                get_request_connection_setup_events_with_latency(client_id, latency_ms).await,
                get_connection_establishment_events_with_latency(client_id, latency_ms).await,
            ];
            for events in collection {
                let reference = events.get_reference();
                if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                    events_analyzer.add_latency_to_collection(&events, &deltas);
                }
            }
        }

        let latencies = events_analyzer.compute_collections_latencies();
        assert_eq!(latencies.len(), 1);
        let latencies = events_analyzer.compute_collections_latencies();
        assert_eq!(latencies.len(), 0);
    }

    #[tokio::test()]
    async fn should_compute_average_latency_considering_all_complete_collections() {
        let compute_average_threshold = 1;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let latency_ms = 10;
        let collections_count = 5;
        for client_id in 0..collections_count {
            let collection = vec![
                get_listener_events_with_latency(client_id, latency_ms).await,
                get_request_connection_setup_events_with_latency(client_id, latency_ms).await,
                get_connection_establishment_events_with_latency(client_id, latency_ms).await,
            ];
            for events in collection {
                let reference = events.get_reference();
                if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                    events_analyzer.add_latency_to_collection(&events, &deltas);
                }
            }
        }

        // if the test fails try to slightly increase 'latency_overhead'
        let latency_overhead = 7;
        let latencies = events_analyzer.compute_collections_latencies();
        assert_eq!(latencies.len(), 1);

        assert_eq!(latencies[0].avg_type, "NewClientConnection");
        assert!(
            latencies[0].value >= Duration::from_millis(3 * latency_ms)
                && latencies[0].value <= Duration::from_millis(3 * latency_ms + latency_overhead)
        );
        // even if the threshold for computing the average is set to 1, as we received more complete collections at the time when "compute_collections_latencies" is called,
        // we compute the average total latency over all the complete collections received
        assert_eq!(latencies[0].count, collections_count as usize);

        let latencies = events_analyzer.compute_collections_latencies();
        assert_eq!(latencies.len(), 0);
    }

    #[tokio::test()]
    async fn should_compute_multiple_average_latencies() {
        let compute_average_threshold = 1;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let canister_id = Principal::anonymous();

        let client_id = 0;
        let connection_latency_ms = 10;
        let new_client_connection_collection = vec![
            get_listener_events_with_latency(client_id, connection_latency_ms).await,
            get_request_connection_setup_events_with_latency(client_id, connection_latency_ms)
                .await,
            get_connection_establishment_events_with_latency(client_id, connection_latency_ms)
                .await,
        ];
        let events_in_new_client_connection_collection =
            new_client_connection_collection.len() as u64;
        for events in new_client_connection_collection {
            let reference = events.get_reference();
            if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                events_analyzer.add_latency_to_collection(&events, &deltas);
            }
        }

        let message_nonce = 1;
        let message_latency_ms = 100;
        let canister_message_collection = vec![
            get_incoming_canister_message_events_with_latency(
                canister_id,
                message_nonce,
                message_latency_ms,
            )
            .await,
            get_outgoing_canister_message_events_with_latency(
                canister_id,
                message_nonce,
                message_latency_ms,
            )
            .await,
        ];
        let events_in_canister_message_collection = canister_message_collection.len() as u64;
        for events in canister_message_collection {
            let reference = events.get_reference();
            if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                events_analyzer.add_latency_to_collection(&events, &deltas);
            }
        }

        let polling_iteration = 2;
        let polling_latency_ms = 500;
        let poller_status_collection = vec![
            get_poller_events_with_latency(canister_id, polling_iteration, polling_latency_ms)
                .await,
        ];
        let events_in_poller_status_collection = poller_status_collection.len() as u64;
        for events in poller_status_collection {
            let reference = events.get_reference();
            if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                events_analyzer.add_latency_to_collection(&events, &deltas);
            }
        }

        // if the test fails try to slightly increase 'latency_overhead'
        let latency_overhead = 7;
        let mut latencies = events_analyzer.compute_collections_latencies();
        // sort is only needed to make sure that the test is stable
        latencies.sort_by(|l1, l2| l1.avg_type.cmp(&l2.avg_type));

        assert_eq!(latencies.len(), 3);

        assert_eq!(latencies[0].avg_type, "CanisterMessage");
        assert!(
            latencies[0].value
                >= Duration::from_millis(
                    events_in_canister_message_collection * message_latency_ms
                )
                && latencies[0].value
                    <= Duration::from_millis(
                        events_in_canister_message_collection * message_latency_ms
                            + latency_overhead
                    )
        );
        assert_eq!(latencies[0].count, compute_average_threshold as usize);

        assert_eq!(latencies[1].avg_type, "NewClientConnection");
        assert!(
            latencies[1].value
                >= Duration::from_millis(
                    events_in_new_client_connection_collection * connection_latency_ms
                )
                && latencies[1].value
                    <= Duration::from_millis(
                        events_in_new_client_connection_collection * connection_latency_ms
                            + latency_overhead
                    )
        );
        assert_eq!(latencies[1].count, compute_average_threshold as usize);

        assert_eq!(latencies[2].avg_type, "PollerStatus");
        assert!(
            latencies[2].value
                >= Duration::from_millis(events_in_poller_status_collection * polling_latency_ms)
                && latencies[2].value
                    <= Duration::from_millis(
                        events_in_poller_status_collection * polling_latency_ms + latency_overhead
                    )
        );
        assert_eq!(latencies[2].count, compute_average_threshold as usize);
    }

    #[tokio::test()]
    async fn should_compute_average_latencies_only_for_complete_collections() {
        let compute_average_threshold = 1;
        let mut events_analyzer = init_events_analyzer(compute_average_threshold);

        let canister_id = Principal::anonymous();

        let client_id = 0;
        let connection_latency_ms = 10;

        let message_nonce = 1;
        let message_latency_ms = 100;

        let polling_iteration = 2;
        let polling_latency_ms = 500;

        for events in vec![
            // complete CanisterMessage and PollerStatus collections
            // incomplete NewClientConnection collection
            get_listener_events_with_latency(client_id, connection_latency_ms).await,
            get_incoming_canister_message_events_with_latency(
                canister_id,
                message_nonce,
                message_latency_ms,
            )
            .await,
            get_request_connection_setup_events_with_latency(client_id, connection_latency_ms)
                .await,
            get_poller_events_with_latency(canister_id, polling_iteration, polling_latency_ms)
                .await,
            get_outgoing_canister_message_events_with_latency(
                canister_id,
                message_nonce,
                message_latency_ms,
            )
            .await,
        ] {
            let reference = events.get_reference();
            if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                events_analyzer.add_latency_to_collection(&events, &deltas);
            }
        }

        // if the test fails try to slightly increase 'latency_overhead'
        let latency_overhead = 7;
        let mut latencies = events_analyzer.compute_collections_latencies();
        // sort is only needed to make sure that the test is stable
        latencies.sort_by(|l1, l2| l1.avg_type.cmp(&l2.avg_type));

        assert_eq!(latencies.len(), 2);

        assert_eq!(latencies[0].avg_type, "CanisterMessage");
        assert!(
            latencies[0].value >= Duration::from_millis(2 * message_latency_ms)
                && latencies[0].value
                    <= Duration::from_millis(2 * message_latency_ms + latency_overhead)
        );
        assert_eq!(latencies[0].count, compute_average_threshold as usize);

        assert_eq!(latencies[1].avg_type, "PollerStatus");
        assert!(
            latencies[1].value >= Duration::from_millis(polling_latency_ms)
                && latencies[1].value
                    <= Duration::from_millis(polling_latency_ms + latency_overhead)
        );
        assert_eq!(latencies[1].count, compute_average_threshold as usize);
    }
}

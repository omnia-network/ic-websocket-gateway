use crate::metrics::canister_poller_metrics::{
    IncomingCanisterMessageEventsMetrics, PollerEventsMetrics,
};
use crate::metrics::client_connection_handler_metrics::{
    OutgoingCanisterMessageEventsMetrics, RequestConnectionSetupEventsMetrics,
};
use crate::metrics::gateway_server_metrics::ConnectionEstablishmentEventsMetrics;
use crate::metrics::ws_listener_metrics::ListenerEventsMetrics;
use std::any::{type_name, Any};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Debug;
use std::{collections::BTreeMap, time::Duration};
use tokio::select;
use tokio::{
    sync::mpsc::{Receiver, Sender},
    time::Instant,
};
use tracing::{error, info, trace, warn};

type EventsType = String;

/// trait implemented by the structs containing the relevant events of each component
pub trait Events: Debug {
    /// returns the name of the struct implementing EventsMetrics
    fn get_metrics_type(&self) -> EventsType {
        self.get_metrics().get_struct_name()
    }

    /// returns the reference used to collect events from different components
    fn get_reference(&self) -> Option<&EventsReference>;

    /// returns the name of the collection which the events in the struct belong to
    fn get_collection_type(&self) -> &EventsCollectionType;

    /// returns the metrics computed from events
    fn get_metrics(&self) -> &dyn EventsMetrics;
}

#[derive(Debug, Clone)]
/// struct used to enforce the following fields on all type aliases implementing Events
pub struct EventsImpl<T: EventsMetrics + Send> {
    /// reference given to events of a component
    /// used to collect it with events of other components
    pub reference: Option<EventsReference>,
    /// name of a collection of events from multiple components
    collection_type: EventsCollectionType,
    /// metrics computed from events of a component
    pub metrics: T,
}

impl<T: EventsMetrics + Send> EventsImpl<T> {
    pub fn new(
        reference: Option<EventsReference>,
        collection_type: EventsCollectionType,
        metrics: T,
    ) -> Self {
        Self {
            reference,
            collection_type,
            metrics,
        }
    }
}

impl<T: EventsMetrics + Send + 'static> Events for EventsImpl<T> {
    fn get_collection_type(&self) -> &EventsCollectionType {
        &self.collection_type
    }

    fn get_reference(&self) -> Option<&EventsReference> {
        if let Some(reference) = &self.reference {
            return Some(reference);
        }
        None
    }

    fn get_metrics(&self) -> &dyn EventsMetrics {
        &self.metrics
    }
}

pub trait EventsMetrics: Debug {
    /// returns the name of the struct
    fn get_struct_name(&self) -> EventsType {
        let path: Vec<String> = type_name::<Self>()
            .split("::")
            .map(|s| s.to_string())
            .collect();
        path.last().expect("not a valid path").to_owned()
    }

    /// returns the value used to compute the time interval between two structs implementing Events
    fn get_value_for_interval(&self) -> &TimeableEvent;

    /// returns the time deltas between the events in the struct implementing Events
    fn compute_deltas(&self, reference: Option<&EventsReference>)
        -> Option<Box<dyn Deltas + Send>>;

    /// computes the interval between two structs implementing Events
    fn compute_interval(&self, previous: &dyn EventsMetrics) -> Duration {
        self.get_value_for_interval()
            .duration_since(previous.get_value_for_interval())
            .expect("previous event must exist")
    }

    /// computes the latency of a component
    fn compute_latency(&self) -> Option<Duration>;
}

/// trait implemented by the structs containing the deltas computed within each component
pub trait Deltas: Debug {
    /// displays all the deltas of an event
    fn display(&self);

    /// returns the reference used to identify the event
    fn get_reference(&self) -> &EventsReference;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventsReference {
    MessageNonce(u64),
    ClientId(u64),
    Iteration(u64),
}

impl EventsReference {
    fn get_inner_value(&self) -> Option<&dyn Any> {
        match self {
            Self::MessageNonce(nonce) => Some(nonce as &dyn Any),
            Self::ClientId(id) => Some(id as &dyn Any),
            Self::Iteration(id) => Some(id as &dyn Any),
        }
    }
}

impl Ord for EventsReference {
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

impl PartialOrd for EventsReference {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum EventsCollectionType {
    NewClientConnection,
    CanisterMessage,
    PollerStatus,
}

impl EventsCollectionType {
    /// returns the events of the components whose latencies affect the relative collection
    /// returns None, if the latency of a collection is irrelevant
    fn get_events_type_in_collection(&self) -> Option<Vec<EventsType>> {
        match self {
            Self::NewClientConnection => Some(vec![
                ListenerEventsMetrics::default().get_struct_name(),
                RequestConnectionSetupEventsMetrics::default().get_struct_name(),
                ConnectionEstablishmentEventsMetrics::default().get_struct_name(),
            ]),
            Self::CanisterMessage => Some(vec![
                IncomingCanisterMessageEventsMetrics::default().get_struct_name(),
                OutgoingCanisterMessageEventsMetrics::default().get_struct_name(),
            ]),
            Self::PollerStatus => Some(vec![PollerEventsMetrics::default().get_struct_name()]),
        }
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

struct EventsData {
    aggregated_metrics_map: BTreeMap<EventsReference, AggregatedMetrics>,
    previous: Box<dyn Events + Send>,
}

impl EventsData {
    fn new(
        aggregated_metrics_map: BTreeMap<EventsReference, AggregatedMetrics>,
        previous: Box<dyn Events + Send>,
    ) -> Self {
        Self {
            aggregated_metrics_map,
            previous,
        }
    }
}

struct EventsLatencies(BTreeSet<(EventsType, Duration)>);

impl EventsLatencies {
    fn default() -> Self {
        Self(BTreeSet::default())
    }

    fn insert(&mut self, events_type: EventsType, latency: Duration) {
        self.0.insert((events_type, latency));
    }

    fn has_received_all_events(&self, events_type_in_collection: &Vec<String>) -> bool {
        let found_event_types: BTreeSet<&EventsType> =
            self.0.iter().map(|(event_type, _)| event_type).collect();
        for event_type in events_type_in_collection {
            if !found_event_types.contains(event_type) {
                return false;
            }
        }
        true
    }

    fn sum(&self) -> Duration {
        self.0.iter().map(|(_, latency)| latency).sum()
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

type CollectionData = BTreeMap<EventsReference, EventsLatencies>;

/// events analyzer receives metrics from different components of the WS Gateway
pub struct EventsAnalyzer {
    /// receiver of the channel used to send metrics to the analyzer
    events_channel_rx: Receiver<Box<dyn Events + Send>>,
    rate_limiting_channel_tx: Sender<f64>,
    min_incoming_interval: u64,
    map_by_events_type: BTreeMap<EventsType, EventsData>,
    map_by_collection_type: HashMap<EventsCollectionType, CollectionData>,
    aggregated_latencies_map: HashMap<EventsCollectionType, BTreeSet<Duration>>,
}

impl EventsAnalyzer {
    pub fn new(
        events_channel_rx: Receiver<Box<dyn Events + Send>>,
        rate_limiting_channel_tx: Sender<f64>,
        min_incoming_interval: u64,
    ) -> Self {
        Self {
            events_channel_rx,
            rate_limiting_channel_tx,
            min_incoming_interval,
            map_by_events_type: BTreeMap::default(),
            map_by_collection_type: HashMap::default(),
            aggregated_latencies_map: HashMap::default(),
        }
    }

    // process the received events
    pub async fn start_processing(&mut self) {
        let periodic_check_operation = periodic_check();
        tokio::pin!(periodic_check_operation);
        loop {
            select! {
                Some(events) = self.events_channel_rx.recv() => {
                    let reference = events.get_reference();
                    if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                        deltas.display();
                        self.add_latency_to_collection(&events, &deltas);
                        self.add_interval_to_events(events, deltas);
                    }
                },
                _ = &mut periodic_check_operation => {
                    self.compute_average_intervals().await;
                    self.compute_collections_latencies();

                    periodic_check_operation.set(periodic_check());
                }
            }
        }
    }

    fn add_latency_to_collection(
        &mut self,
        events: &Box<dyn Events + Send>,
        deltas: &Box<dyn Deltas + Send>,
    ) {
        let events_type = events.get_metrics_type();
        let collection_type = events.get_collection_type();
        let latency = deltas.get_latency();
        let reference = deltas.get_reference();

        if let Some(collection_data) = self.map_by_collection_type.get_mut(collection_type) {
            if let Some(latencies) = collection_data.get_mut(reference) {
                latencies.insert(events_type, latency);
            } else {
                let mut latencies = EventsLatencies::default();
                latencies.insert(events_type, latency);
                collection_data.insert(reference.to_owned(), latencies);
            }
        } else {
            let mut latencies = EventsLatencies::default();
            latencies.insert(events_type, latency);
            let mut latencies_map = CollectionData::default();
            latencies_map.insert(reference.to_owned(), latencies);
            let collection_data = latencies_map;
            self.map_by_collection_type
                .insert(collection_type.to_owned(), collection_data);
        }
    }

    fn add_interval_to_events(
        &mut self,
        events: Box<dyn Events + Send>,
        deltas: Box<dyn Deltas + Send>,
    ) {
        let events_type = events.get_metrics_type();
        let reference = deltas.get_reference().to_owned();

        // first events received for each type is not processed further as there is no previous event for computing interval
        if let Some(data) = self.map_by_events_type.get_mut(&events_type) {
            let aggregated_metrics = AggregatedMetrics::new(
                deltas,
                events
                    .get_metrics()
                    .compute_interval(data.previous.get_metrics()),
            );
            data.aggregated_metrics_map
                .insert(reference, aggregated_metrics);
            data.previous = events;
        } else {
            let data = EventsData::new(BTreeMap::default(), events);
            self.map_by_events_type.insert(events_type, data);
        }
    }

    async fn compute_average_intervals(&mut self) {
        let min_incoming_interval = self.min_incoming_interval;
        for (events_type, events_data) in self.map_by_events_type.iter_mut() {
            // TODO: rolling average
            if events_data.aggregated_metrics_map.len() > 10 {
                let intervals = events_data.aggregated_metrics_map.iter().fold(
                    Vec::new(),
                    |mut intervals, (_, aggregated_metrics)| {
                        trace!(
                            "Deltas for {:?}: {:?}",
                            events_type,
                            aggregated_metrics.deltas
                        );
                        intervals.push(aggregated_metrics.interval);
                        intervals
                    },
                );
                let sum_intervals: Duration = intervals.iter().sum();
                let avg_interval = sum_intervals.div_f64(intervals.len() as f64);
                info!(
                    "Average interval for {:?}: {:?} computed over: {:?} metrics",
                    events_type,
                    avg_interval,
                    intervals.len()
                );
                let limiting_rate;
                if String::from("RequestConnectionSetupEventsMetrics").eq(events_type) {
                    if avg_interval < Duration::from_millis(min_incoming_interval) {
                        warn!("Signaling WS listener for rate limiting due to too many incoming connections. Average interval {:?}", avg_interval);
                        limiting_rate = get_limiting_rate(min_incoming_interval, avg_interval);
                    } else {
                        // if the average interval of incoming connections is above the minimum (MIN_INCOMING_INTERVAL)
                        // the listener should accept all connections (limiting_rate = 0)
                        limiting_rate = 0.0;
                    }
                    if let Err(e) = self.rate_limiting_channel_tx.send(limiting_rate).await {
                        error!("Rate limiting channel closed on the receiver side: {:?}", e);
                    }
                }

                events_data.aggregated_metrics_map = BTreeMap::default();
            }
        }
    }

    fn compute_collections_latencies(&mut self) {
        for (collection_type, collection_data) in self.map_by_collection_type.iter_mut() {
            if let Some(events_type_in_collection) = collection_type.get_events_type_in_collection()
            {
                for (_events_reference, latencies) in collection_data.iter_mut() {
                    if latencies.has_received_all_events(&events_type_in_collection) {
                        let total_latency: Duration = latencies.sum();
                        latencies.clear();
                        if let Some(aggregated_latencies) =
                            self.aggregated_latencies_map.get_mut(collection_type)
                        {
                            if aggregated_latencies.len() > 10 {
                                let sum_latencies: Duration = aggregated_latencies.iter().sum();
                                let avg_latencies =
                                    sum_latencies.div_f64(aggregated_latencies.len() as f64);
                                info!(
                                    "Average total latency of events in collection: {:?}: {:?} computed over: {:?} collections",
                                    collection_type, avg_latencies, aggregated_latencies.len()
                                );
                                aggregated_latencies.clear();
                            } else {
                                aggregated_latencies.insert(total_latency);
                            }
                        } else {
                            let mut aggregated_latencies: BTreeSet<_> = BTreeSet::default();
                            aggregated_latencies.insert(total_latency);
                            self.aggregated_latencies_map
                                .insert(collection_type.to_owned(), aggregated_latencies);
                        }
                    }
                }
            }
        }
    }
}

async fn periodic_check() {
    tokio::time::sleep(Duration::from_secs(5)).await;
}

fn get_limiting_rate(min_incoming_interval: u64, avg_interval: Duration) -> f64 {
    (min_incoming_interval as u64 - avg_interval.as_millis() as u64) as f64
        / min_incoming_interval as f64
}

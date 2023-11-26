use crate::metrics::canister_poller_metrics::{
    IncomingCanisterMessageEventsMetrics, PollerEventsMetrics,
};
use crate::metrics::client_session_handler_metrics::{
    OutgoingCanisterMessageEventsMetrics, RequestConnectionSetupEventsMetrics,
};
use crate::metrics::manager_metrics::ConnectionEstablishmentEventsMetrics;
use crate::metrics::ws_listener_metrics::ListenerEventsMetrics;
use crate::ws_listener::ClientId;
use candid::Principal;
use std::any::{type_name, Any};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::fmt::{self, Debug};
use std::{collections::BTreeMap, time::Duration};
use tokio::select;
use tokio::{
    sync::mpsc::{Receiver, Sender},
    time::Instant,
};
use tracing::{error, info, warn};

/// name of the struct implementing EventsMetrics
pub type EventsType = String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    principal: Principal,
    nonce: u64,
}

impl Reference {
    pub fn new(principal: Principal, nonce: u64) -> Self {
        Self { principal, nonce }
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}_{}", self.principal, self.nonce)
    }
}

pub type MessageReference = Reference;

pub type IterationReference = Reference;

/// trait implemented by the structs containing the relevant events of each component
pub trait Events: Debug {
    /// returns the name of the struct implementing EventsMetrics
    fn get_metrics_type(&self) -> EventsType {
        self.get_metrics().get_struct_name()
    }

    /// returns the reference used to collect events groups from different components in the same collection
    fn get_reference(&self) -> Option<&EventsReference>;

    /// returns the name of the collection which the events groups in the struct belong to
    fn get_collection_type(&self) -> &EventsCollectionType;

    /// returns the metrics computed from an events group
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
    /// displays all the deltas of an events group
    fn display(&self);

    /// returns the reference used to identify a delta computed from an events group
    fn get_reference(&self) -> &EventsReference;

    /// returns the latency of an events group
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
/// reference used to identify an events group
pub enum EventsReference {
    /// reference of an events group related to an incoming or outgoing message
    MessageReference(MessageReference),
    /// reference of an events group related to a client connection
    // cannot use ClientKey as it is not known by 'ws_listener'
    ClientId(ClientId),
    /// reference of an events group related to a poller
    IterationReference(IterationReference),
}

impl EventsReference {
    /// returns the value wrappd by the enum variant
    fn get_inner_value(&self) -> Option<&dyn Any> {
        match self {
            Self::MessageReference(nonce) => Some(nonce as &dyn Any),
            Self::ClientId(id) => Some(id as &dyn Any),
            Self::IterationReference(id) => Some(id as &dyn Any),
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
/// possible collections which events groups belong to
pub enum EventsCollectionType {
    /// collection of events groups related to a new client connection
    NewClientConnection,
    /// collection of events groups related to a incoming or outgoing canister message
    CanisterMessage,
    /// collection of events groups related to a poller
    PollerStatus,
}

impl fmt::Display for EventsCollectionType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::NewClientConnection => write!(f, "NewClientConnection"),
            Self::CanisterMessage => write!(f, "CanisterMessage"),
            Self::PollerStatus => write!(f, "PollerStatus"),
        }
    }
}

impl EventsCollectionType {
    /// returns the types of the events groups that must be found in a collection to be considered complete,
    /// to then compute the total collection latency
    fn get_expected_events_type_in_collection(&self) -> Option<Vec<EventsType>> {
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

pub struct IntervalsForEventsType {
    pub intervals: BTreeSet<Duration>,
    previous_events_group: Box<dyn Events + Send>,
}

impl IntervalsForEventsType {
    fn new(intervals: BTreeSet<Duration>, previous_events_group: Box<dyn Events + Send>) -> Self {
        Self {
            intervals,
            previous_events_group,
        }
    }

    fn sum(&self) -> Duration {
        self.intervals.iter().sum()
    }
}

/// set of latencies - and their events type - for events groups with the same reference
/// for a given events reference,once all the events expected for the corresponding collection are recorded, the sum of these latencies gives the total collection latency
pub struct EventsLatencies(BTreeSet<(EventsType, Duration)>);

impl EventsLatencies {
    fn default() -> Self {
        Self(BTreeSet::default())
    }

    #[cfg(test)]
    pub fn get_inner(&self) -> &BTreeSet<(EventsType, Duration)> {
        &self.0
    }

    fn insert(&mut self, events_type: EventsType, latency: Duration) {
        self.0.insert((events_type, latency));
    }

    /// checks if all the events groups, with the same reference, expected for the corresponding collection have already been recorded
    fn has_received_all_events(&self, expected_events_type_in_collection: &Vec<String>) -> bool {
        let recorded_event_types: BTreeSet<&EventsType> =
            self.0.iter().map(|(event_type, _)| event_type).collect();
        // for each event type expected in the collection, check if it has been recorded
        for event_type in expected_events_type_in_collection {
            if !recorded_event_types.contains(event_type) {
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

/// for a given collection, collects all the latencies for events groups with the same reference
type CollectionLatencies = BTreeMap<EventsReference, EventsLatencies>;

pub struct AverageData {
    pub avg_type: String,
    pub value: Duration,
    pub count: usize,
}

/// events analyzer receives metrics from different components of the WS Gateway
pub struct EventsAnalyzer {
    /// receiver side of the channel used to send metrics to the analyzer
    analyzer_channel_rx: Receiver<Box<dyn Events + Send>>,
    /// sender side of the channel used to send the limiting rate to the WS listener
    rate_limiting_channel_tx: Sender<Option<f64>>,
    /// minimum interval between consecutive incoming connections
    /// if below this threshold, rate limiting will start
    /// proportionally to the difference between the measured interval and the threshold
    min_incoming_interval: u64,
    /// threshold after which we compute the averages of the intervals/latencies
    compute_averages_threshold: u64,
    /// maps the type of an events group to the intervals computed from consecutive events groups of that type
    pub map_intervals_by_events_type: BTreeMap<EventsType, IntervalsForEventsType>,
    /// maps the type of a collection to a map of containing all the recorded latencies for events groups with the same reference
    pub map_latencies_by_collection_type: HashMap<EventsCollectionType, CollectionLatencies>,
    pub aggregated_latencies_map: HashMap<EventsCollectionType, BTreeSet<Duration>>,
}

impl EventsAnalyzer {
    pub fn new(
        analyzer_channel_rx: Receiver<Box<dyn Events + Send>>,
        rate_limiting_channel_tx: Sender<Option<f64>>,
        min_incoming_interval: u64,
        compute_averages_threshold: u64,
    ) -> Self {
        Self {
            analyzer_channel_rx,
            rate_limiting_channel_tx,
            min_incoming_interval,
            compute_averages_threshold,
            map_intervals_by_events_type: BTreeMap::default(),
            map_latencies_by_collection_type: HashMap::default(),
            aggregated_latencies_map: HashMap::default(),
        }
    }

    // process the received events
    pub async fn start_processing(&mut self) {
        let periodic_check_operation = periodic_check();
        tokio::pin!(periodic_check_operation);
        loop {
            select! {
                // register each event received on the channel for periodic processing
                Some(events) = self.analyzer_channel_rx.recv() => {
                    let reference = events.get_reference();
                    if let Some(deltas) = events.get_metrics().compute_deltas(reference) {
                        deltas.display();
                        self.add_latency_to_collection(&events, &deltas);
                        self.add_interval_to_events(events);
                    }
                },
                // periodically process previously registered events
                _ = &mut periodic_check_operation => {
                    let intervals = self.compute_average_intervals();
                    for avg_interval in intervals {
                        info!(
                            "Average interval for {:?}: {:?} computed over: {:?} intervals",
                            avg_interval.avg_type, avg_interval.value, avg_interval.count
                        );
                        // if we computed the average interval for events groups representing the frequency of incoming connections, compute limiting rate
                        if String::from("RequestConnectionSetupEventsMetrics").eq(&avg_interval.avg_type) {
                            // if the average interval of incoming connections is above the minimum threshold
                            // the listener should accept all connections (limiting_rate = None)
                            let mut limiting_rate = None;
                            if avg_interval.value < Duration::from_millis(self.min_incoming_interval) {
                                warn!("Signaling WS listener for rate limiting due to too many incoming connections. Average interval {:?}", avg_interval.value);
                                limiting_rate =
                                    Some(get_limiting_rate(self.min_incoming_interval, avg_interval.value));
                            }
                            if let Err(e) = self.rate_limiting_channel_tx.send(limiting_rate).await {
                                error!("Rate limiting channel closed on the receiver side: {:?}", e);
                            }
                        }
                    }

                    let latencies = self.compute_collections_latencies();
                    for avg_latency in latencies {
                        info!(
                            "Average total latency of events in collection: {:?}: {:?} computed over: {:?} collections",
                            avg_latency.avg_type, avg_latency.value, avg_latency.count
                        );
                    }

                    periodic_check_operation.set(periodic_check());
                }
            }
        }
    }

    /// deltas computed from an events group are collected by reference in the collection type they belong to
    pub fn add_latency_to_collection(
        &mut self,
        events: &Box<dyn Events + Send>,
        deltas: &Box<dyn Deltas + Send>,
    ) {
        let events_type = events.get_metrics_type();
        let collection_type = events.get_collection_type();
        let latency = deltas.get_latency();
        let reference = deltas.get_reference();

        if let Some(collection_latencies) = self
            .map_latencies_by_collection_type
            .get_mut(collection_type)
        {
            if let Some(latencies) = collection_latencies.get_mut(reference) {
                // inserts the events group type and the corresponding latency of the delta computed from the events group into its correpsonding collection
                // this will contain all the other latencies computed from the events groups with the same reference and collection type
                latencies.insert(events_type, latency);
            } else {
                let mut latencies = EventsLatencies::default();
                latencies.insert(events_type, latency);
                collection_latencies.insert(reference.to_owned(), latencies);
            }
        } else {
            let mut latencies = EventsLatencies::default();
            latencies.insert(events_type, latency);
            let mut latencies_map = CollectionLatencies::default();
            latencies_map.insert(reference.to_owned(), latencies);
            let collection_latencies = latencies_map;
            self.map_latencies_by_collection_type
                .insert(collection_type.to_owned(), collection_latencies);
        }

        // check if all the events groups expected from the collection have been recorded
        // get the expected events type in the collection
        let latencies = self
            .map_latencies_by_collection_type
            .get_mut(collection_type)
            .expect("should have been created above")
            .get_mut(reference)
            .expect("should have been created above");
        if let Some(expected_events_type_in_collection) =
            collection_type.get_expected_events_type_in_collection()
        {
            if latencies.has_received_all_events(&expected_events_type_in_collection) {
                // if all the events groups expected from the collection have been recorded, compute the total latency of the collection
                let total_latency = latencies.sum();
                if let Some(aggregated_latencies) =
                    self.aggregated_latencies_map.get_mut(collection_type)
                {
                    aggregated_latencies.insert(total_latency);
                } else {
                    let mut aggregated_latencies: BTreeSet<_> = BTreeSet::default();
                    aggregated_latencies.insert(total_latency);
                    self.aggregated_latencies_map
                        .insert(collection_type.to_owned(), aggregated_latencies);
                }
                latencies.clear();
            }
        }
    }

    pub fn add_interval_to_events(&mut self, events: Box<dyn Events + Send>) {
        let events_type = events.get_metrics_type();
        // first events group received for each type is not processed further as there is no previous event for computing interval
        if let Some(data) = self.map_intervals_by_events_type.get_mut(&events_type) {
            let interval = events
                .get_metrics()
                .compute_interval(data.previous_events_group.get_metrics());
            data.intervals.insert(interval);
            data.previous_events_group = events;
        } else {
            let data = IntervalsForEventsType::new(BTreeSet::default(), events);
            self.map_intervals_by_events_type.insert(events_type, data);
        }
    }

    /// computes the average of the time between two consecutive events groups of the same type, for each type
    pub fn compute_average_intervals(&mut self) -> Vec<AverageData> {
        let mut intervals = Vec::new();
        for (events_type, events_intervals) in self.map_intervals_by_events_type.iter_mut() {
            let intervals_count = events_intervals.intervals.len();
            if intervals_count as u64 >= self.compute_averages_threshold {
                // if we recorded at least 10 intervals from events groups of the same type, compute the average interval
                let sum_intervals = events_intervals.sum();
                let avg_interval = sum_intervals.div_f64(intervals_count as f64);
                intervals.push(AverageData {
                    avg_type: events_type.to_owned(),
                    value: avg_interval,
                    count: intervals_count,
                });
                events_intervals.intervals = BTreeSet::default();
            }
        }
        intervals
    }

    /// computes the average of the total latency of events in each collection
    pub fn compute_collections_latencies(&mut self) -> Vec<AverageData> {
        let mut latencies = Vec::new();
        for (collection_type, aggregated_latencies) in self.aggregated_latencies_map.iter_mut() {
            if aggregated_latencies.len() as u64 >= self.compute_averages_threshold {
                let sum_latencies: Duration = aggregated_latencies.iter().sum();
                let avg_latencies = sum_latencies.div_f64(aggregated_latencies.len() as f64);
                latencies.push(AverageData {
                    avg_type: collection_type.to_owned().to_string(),
                    value: avg_latencies,
                    count: aggregated_latencies.len(),
                });
                aggregated_latencies.clear();
            }
        }
        latencies
    }
}

async fn periodic_check() {
    tokio::time::sleep(Duration::from_secs(5)).await;
}

fn get_limiting_rate(min_incoming_interval: u64, avg_interval: Duration) -> f64 {
    (min_incoming_interval as u64 - avg_interval.as_millis() as u64) as f64
        / min_incoming_interval as f64
}

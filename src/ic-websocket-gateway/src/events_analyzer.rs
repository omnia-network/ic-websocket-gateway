use std::any::{type_name, Any};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Debug;
use std::{collections::BTreeMap, time::Duration};
use tokio::select;
use tokio::{sync::mpsc::Receiver, time::Instant};
use tracing::{debug, info};

type EventsType = String;

/// trait implemented by the structs containing the relevant events of each component
pub trait Events: Debug {
    fn get_type(&self) -> EventsType {
        let path: Vec<String> = type_name::<Self>()
            .split("::")
            .map(|s| s.to_string())
            .collect();
        path.last().expect("not a valid path").to_owned()
    }

    /// returns the name of the collection which the events in the struct belong to
    fn get_collection_type(&self) -> EventsCollectionType;

    /// returns the value used to compute the time interval between two structs implementing Events
    fn get_value_for_interval(&self) -> &TimeableEvent;

    /// returns the time deltas between the events in the struct implementing Events
    fn compute_deltas(&self) -> Option<Box<dyn Deltas + Send>>;

    /// computes the interval between two structs implementing Events
    fn compute_interval(&self, previous: &Box<dyn Events + Send>) -> Duration {
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

    /// returns the latency of the component
    fn get_latency(&self) -> Duration;

    /// returns the reference used to identify the event
    fn get_reference(&self) -> &EventsReference;
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

#[derive(Debug, PartialEq, Eq, Hash)]
pub enum EventsCollectionType {
    NewClientConnection,
    CanisterMessage,
    PollerStatus,
}

impl EventsCollectionType {
    /// returns the number of components whose latencies affect the relative collection
    /// returns None, if the latency of a collection is irrelevant
    fn get_collection_size(&self) -> Option<usize> {
        match self {
            Self::NewClientConnection => Some(2), // should be 3 once we measure also the GW state events
            Self::CanisterMessage => Some(1), // should be 2 once we measure also the handler events for outgoing messages
            Self::PollerStatus => Some(1),
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

type EventsLatencies = BTreeSet<Duration>;
type CollectionData = BTreeMap<EventsReference, EventsLatencies>;

/// events analyzer receives metrics from different components of the WS Gateway
pub struct EventsAnalyzer {
    /// receiver of the channel used to send metrics to the analyzer
    events_channel_rx: Receiver<Box<dyn Events + Send>>,
    map_by_events_type: BTreeMap<EventsType, EventsData>,
    map_by_collection_type: HashMap<EventsCollectionType, CollectionData>,
}

impl EventsAnalyzer {
    pub fn new(events_channel_rx: Receiver<Box<dyn Events + Send>>) -> Self {
        Self {
            events_channel_rx,
            map_by_events_type: BTreeMap::default(),
            map_by_collection_type: HashMap::default(),
        }
    }

    // process the received events
    pub async fn start_processing(&mut self) {
        let periodic_check_operation = periodic_check();
        tokio::pin!(periodic_check_operation);
        loop {
            select! {
                Some(events) = self.events_channel_rx.recv() => {
                    if let Some(deltas) = events.compute_deltas() {
                        deltas.display();
                        self.add_latency_to_collection(&events, &deltas);
                        self.add_interval_to_events(events, deltas);
                    }
                },
                _ = &mut periodic_check_operation => {
                    self.compute_average_intervals();
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
        let collection_type = events.get_collection_type();
        let latency = deltas.get_latency();
        let reference = deltas.get_reference().clone();

        if let Some(collection_data) = self.map_by_collection_type.get_mut(&collection_type) {
            if let Some(latencies) = collection_data.get_mut(&reference) {
                latencies.insert(latency);
            } else {
                let mut latencies = EventsLatencies::default();
                latencies.insert(latency);
                collection_data.insert(reference, latencies);
            }
        } else {
            let mut latencies = EventsLatencies::default();
            latencies.insert(latency);
            let mut latencies_map = CollectionData::default();
            latencies_map.insert(reference, latencies);
            let collection_data = latencies_map;
            self.map_by_collection_type
                .insert(collection_type, collection_data);
        }
    }

    fn add_interval_to_events(
        &mut self,
        events: Box<dyn Events + Send>,
        deltas: Box<dyn Deltas + Send>,
    ) {
        let events_type = events.get_type();
        let reference = deltas.get_reference().clone();

        // first events received for each type is not processed further as there is no previous event for computing interval
        if let Some(data) = self.map_by_events_type.get_mut(&events_type) {
            let aggregated_metrics =
                AggregatedMetrics::new(deltas, events.compute_interval(&data.previous));
            data.aggregated_metrics_map
                .insert(reference, aggregated_metrics);
            data.previous = events;
        } else {
            let data = EventsData::new(BTreeMap::default(), events);
            self.map_by_events_type.insert(events_type, data);
        }
    }

    fn compute_average_intervals(&mut self) {
        for (events_type, events_data) in self.map_by_events_type.iter_mut() {
            if events_data.aggregated_metrics_map.len() > 10 {
                let intervals = events_data.aggregated_metrics_map.iter().fold(
                    Vec::new(),
                    |mut intervals, (_, aggregated_metrics)| {
                        debug!(
                            "Deltas for {:?}: {:?}",
                            events_type, aggregated_metrics.deltas
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

                events_data.aggregated_metrics_map = BTreeMap::default();
            }
        }
    }

    fn compute_collections_latencies(&mut self) {
        for (collection_type, collection_data) in self.map_by_collection_type.iter_mut() {
            if let Some(collection_size) = collection_type.get_collection_size() {
                for (events_reference, latencies) in collection_data.iter_mut() {
                    if latencies.len() == collection_size {
                        let total_latency: Duration = latencies.iter().sum();
                        info!("Total latency for events with reference {:?} for collection of type: {:?}: {:?}", events_reference, collection_type, total_latency);
                        latencies.clear();
                    }
                }
            }
        }
    }
}

async fn periodic_check() {
    tokio::time::sleep(Duration::from_secs(5)).await;
}
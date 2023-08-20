use crate::events_analyzer::{Deltas, EventsImpl, EventsMetrics, EventsReference, TimeableEvent};
use std::time::Duration;
use tracing::debug;

pub type ListenerEvents = EventsImpl<ListenerEventsMetrics>;

#[derive(Debug, Clone)]
pub struct ListenerEventsMetrics {
    received_request: TimeableEvent,
    accepted_with_tls: TimeableEvent,
    accepted_without_tls: TimeableEvent,
    started_handler: TimeableEvent,
}

impl ListenerEventsMetrics {
    pub fn default() -> Self {
        Self {
            received_request: TimeableEvent::default(),
            accepted_with_tls: TimeableEvent::default(),
            accepted_without_tls: TimeableEvent::default(),
            started_handler: TimeableEvent::default(),
        }
    }

    pub fn set_received_request(&mut self) {
        self.received_request.set_now()
    }

    pub fn set_accepted_with_tls(&mut self) {
        self.accepted_with_tls.set_now()
    }

    pub fn set_accepted_without_tls(&mut self) {
        self.accepted_without_tls.set_now()
    }

    pub fn set_started_handler(&mut self) {
        self.started_handler.set_now()
    }
}

impl EventsMetrics for ListenerEventsMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.received_request
    }

    fn compute_deltas(&self, reference: Option<EventsReference>) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let accepted = {
                if self.accepted_with_tls.is_set() {
                    self.accepted_with_tls.clone()
                } else {
                    self.accepted_without_tls.clone()
                }
            };
            let time_to_accept = accepted.duration_since(&self.received_request)?;
            let time_to_start_handling = self.started_handler.duration_since(&accepted)?;
            let latency = self.compute_latency()?;

            return Some(Box::new(ListenerDeltas::new(
                reference,
                time_to_accept,
                time_to_start_handling,
                latency,
            )));
        }
        None
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.started_handler.duration_since(&self.received_request)
    }
}

#[derive(Debug)]
struct ListenerDeltas {
    reference: EventsReference,
    time_to_accept: Duration,
    time_to_start_handling: Duration,
    latency: Duration,
}

impl ListenerDeltas {
    fn new(
        reference: EventsReference,
        time_to_accept: Duration,
        time_to_start_handling: Duration,
        latency: Duration,
    ) -> Self {
        Self {
            reference,
            time_to_accept,
            time_to_start_handling,
            latency,
        }
    }
}

impl Deltas for ListenerDeltas {
    fn display(&self) {
        debug!(
            "\nreference: {:?}\ntime_to_accept: {:?}\ntime_to_start_handling: {:?}\nlatency: {:?}",
            self.reference, self.time_to_accept, self.time_to_start_handling, self.latency
        );
    }

    fn get_reference(&self) -> &EventsReference {
        &self.reference
    }

    fn get_latency(&self) -> Duration {
        self.latency
    }
}

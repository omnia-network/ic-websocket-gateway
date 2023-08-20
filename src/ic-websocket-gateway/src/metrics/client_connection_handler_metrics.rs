use crate::events_analyzer::{Deltas, EventsImpl, EventsMetrics, EventsReference, TimeableEvent};
use std::time::Duration;
use tracing::debug;

pub type ConnectionSetupEvents = EventsImpl<ConnectionSetupEventsMetrics>;

#[derive(Debug, Clone)]
pub struct ConnectionSetupEventsMetrics {
    accepted_ws_connection: TimeableEvent,
    received_first_message: TimeableEvent,
    validated_first_message: TimeableEvent,
    established_ws_connection: TimeableEvent,
}

impl ConnectionSetupEventsMetrics {
    pub fn default() -> Self {
        Self {
            accepted_ws_connection: TimeableEvent::default(),
            received_first_message: TimeableEvent::default(),
            validated_first_message: TimeableEvent::default(),
            established_ws_connection: TimeableEvent::default(),
        }
    }

    pub fn set_accepted_ws_connection(&mut self) {
        self.accepted_ws_connection.set_now();
    }

    pub fn set_received_first_message(&mut self) {
        self.received_first_message.set_now();
    }

    pub fn set_validated_first_message(&mut self) {
        self.validated_first_message.set_now();
    }

    pub fn set_established_ws_connection(&mut self) {
        self.established_ws_connection.set_now();
    }
}

impl EventsMetrics for ConnectionSetupEventsMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.established_ws_connection
    }

    fn compute_deltas(&self, reference: Option<EventsReference>) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let time_to_first_message = self
                .received_first_message
                .duration_since(&self.accepted_ws_connection)?;
            let time_to_validation = self
                .validated_first_message
                .duration_since(&self.received_first_message)?;
            let time_to_establishment = self
                .established_ws_connection
                .duration_since(&self.validated_first_message)?;
            let latency = self.compute_latency()?;

            return Some(Box::new(ConnectionSetupDeltas::new(
                reference,
                time_to_first_message,
                time_to_validation,
                time_to_establishment,
                latency,
            )));
        }
        None
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.established_ws_connection
            .duration_since(&self.accepted_ws_connection)
    }
}

#[derive(Debug)]
struct ConnectionSetupDeltas {
    reference: EventsReference,
    time_to_first_message: Duration,
    time_to_validation: Duration,
    time_to_establishment: Duration,
    latency: Duration,
}

impl ConnectionSetupDeltas {
    pub fn new(
        reference: EventsReference,
        time_to_first_message: Duration,
        time_to_validation: Duration,
        time_to_establishment: Duration,
        latency: Duration,
    ) -> Self {
        Self {
            reference,
            time_to_first_message,
            time_to_validation,
            time_to_establishment,
            latency,
        }
    }
}

impl Deltas for ConnectionSetupDeltas {
    fn display(&self) {
        debug!(
            "\nreference: {:?}\ntime_to_first_message: {:?}\ntime_to_validation: {:?}\ntime_to_establishment: {:?}\nlatency: {:?}",
            self.reference, self.time_to_first_message, self.time_to_validation, self.time_to_establishment, self.latency
        );
    }

    fn get_reference(&self) -> &EventsReference {
        &self.reference
    }

    fn get_latency(&self) -> Duration {
        self.latency
    }
}

use crate::events_analyzer::{Deltas, EventsImpl, EventsMetrics, EventsReference, TimeableEvent};
use std::time::Duration;
use tracing::trace;

pub type RequestConnectionSetupEvents = EventsImpl<RequestConnectionSetupEventsMetrics>;

#[derive(Debug, Clone)]
pub struct RequestConnectionSetupEventsMetrics {
    accepted_ws_connection: TimeableEvent,
    ws_connection_setup: TimeableEvent,
}

impl RequestConnectionSetupEventsMetrics {
    pub fn default() -> Self {
        Self {
            accepted_ws_connection: TimeableEvent::default(),
            ws_connection_setup: TimeableEvent::default(),
        }
    }

    pub fn set_accepted_ws_connection(&mut self) {
        self.accepted_ws_connection.set_now();
    }

    pub fn set_ws_connection_setup(&mut self) {
        self.ws_connection_setup.set_now();
    }
}

impl EventsMetrics for RequestConnectionSetupEventsMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.ws_connection_setup
    }

    fn compute_deltas(
        &self,
        reference: Option<&EventsReference>,
    ) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let time_to_setup = self
                .ws_connection_setup
                .duration_since(&self.accepted_ws_connection)?;
            let latency = self.compute_latency()?;

            return Some(Box::new(RequestConnectionSetupDeltas::new(
                reference.to_owned(),
                time_to_setup,
                latency,
            )));
        }
        None
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.ws_connection_setup
            .duration_since(&self.accepted_ws_connection)
    }
}

#[derive(Debug)]
struct RequestConnectionSetupDeltas {
    reference: EventsReference,
    time_to_setup: Duration,
    latency: Duration,
}

impl RequestConnectionSetupDeltas {
    pub fn new(reference: EventsReference, time_to_setup: Duration, latency: Duration) -> Self {
        Self {
            reference,
            time_to_setup,
            latency,
        }
    }
}

impl Deltas for RequestConnectionSetupDeltas {
    fn display(&self) {
        trace!(
            "reference: {:?}, time_to_setup: {:?}, latency: {:?}",
            self.reference,
            self.time_to_setup,
            self.latency
        );
    }

    fn get_reference(&self) -> &EventsReference {
        &self.reference
    }

    fn get_latency(&self) -> Duration {
        self.latency
    }
}

pub type OutgoingCanisterMessageEvents = EventsImpl<OutgoingCanisterMessageEventsMetrics>;

#[derive(Debug, Clone)]
pub struct OutgoingCanisterMessageEventsMetrics {
    received_canister_message: TimeableEvent,
    message_sent_to_client: TimeableEvent,
}

impl OutgoingCanisterMessageEventsMetrics {
    pub fn default() -> Self {
        Self {
            received_canister_message: TimeableEvent::default(),
            message_sent_to_client: TimeableEvent::default(),
        }
    }

    pub fn set_received_canister_message(&mut self) {
        self.received_canister_message.set_now();
    }

    pub fn set_message_sent_to_client(&mut self) {
        self.message_sent_to_client = TimeableEvent::now();
    }

    pub fn set_no_message_sent_to_client(&mut self) {
        self.message_sent_to_client = TimeableEvent::default();
    }
}

impl EventsMetrics for OutgoingCanisterMessageEventsMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.received_canister_message
    }

    fn compute_deltas(
        &self,
        reference: Option<&EventsReference>,
    ) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let time_to_send = self
                .message_sent_to_client
                .duration_since(&self.received_canister_message)?;
            let latency = self.compute_latency()?;

            return Some(Box::new(OutgoingCanisterMessageDeltas::new(
                reference.to_owned(),
                time_to_send,
                latency,
            )));
        }
        None
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.message_sent_to_client
            .duration_since(&self.received_canister_message)
    }
}

#[derive(Debug)]
struct OutgoingCanisterMessageDeltas {
    reference: EventsReference,
    time_to_send: Duration,
    latency: Duration,
}

impl OutgoingCanisterMessageDeltas {
    pub fn new(reference: EventsReference, time_to_send: Duration, latency: Duration) -> Self {
        Self {
            reference,
            time_to_send,
            latency,
        }
    }
}

impl Deltas for OutgoingCanisterMessageDeltas {
    fn display(&self) {
        trace!(
            "reference: {:?}, time_to_send: {:?}, latency: {:?}",
            self.reference,
            self.time_to_send,
            self.latency
        );
    }

    fn get_reference(&self) -> &EventsReference {
        &self.reference
    }

    fn get_latency(&self) -> Duration {
        self.latency
    }
}

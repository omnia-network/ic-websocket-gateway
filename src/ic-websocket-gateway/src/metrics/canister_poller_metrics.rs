use crate::events_analyzer::{Deltas, EventsImpl, EventsMetrics, EventsReference, TimeableEvent};
use std::time::Duration;
use tracing::trace;

pub type PollerEvents = EventsImpl<PollerEventsMetrics>;

#[derive(Debug, Clone)]
pub struct PollerEventsMetrics {
    start_polling: TimeableEvent,
    received_messages: TimeableEvent,
    start_relaying_messages: TimeableEvent,
    finished_relaying_messages: TimeableEvent,
}

impl PollerEventsMetrics {
    pub fn default() -> Self {
        Self {
            start_polling: TimeableEvent::default(),
            received_messages: TimeableEvent::default(),
            start_relaying_messages: TimeableEvent::default(),
            finished_relaying_messages: TimeableEvent::default(),
        }
    }

    pub fn set_start_polling(&mut self) {
        self.start_polling.set_now();
    }

    pub fn set_received_messages(&mut self) {
        self.received_messages.set_now();
    }

    pub fn set_start_relaying_messages(&mut self) {
        self.start_relaying_messages.set_now();
    }

    pub fn set_finished_relaying_messages(&mut self) {
        self.finished_relaying_messages.set_now();
    }
}

impl EventsMetrics for PollerEventsMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.received_messages
    }

    fn compute_deltas(
        &self,
        reference: Option<&EventsReference>,
    ) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let time_to_receive = self.received_messages.duration_since(&self.start_polling)?;
            let time_to_start_relaying = self
                .start_relaying_messages
                .duration_since(&self.received_messages)?;
            let time_to_relay = self
                .finished_relaying_messages
                .duration_since(&self.start_relaying_messages)?;
            let latency = self.compute_latency()?;

            return Some(Box::new(PollerDeltas::new(
                reference.to_owned(),
                time_to_receive,
                time_to_start_relaying,
                time_to_relay,
                latency,
            )));
        }
        None
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.finished_relaying_messages
            .duration_since(&self.start_polling)
    }
}

#[derive(Debug)]
struct PollerDeltas {
    reference: EventsReference,
    time_to_receive: Duration,
    time_to_start_relaying: Duration,
    time_to_relay: Duration,
    latency: Duration,
}

impl PollerDeltas {
    pub fn new(
        reference: EventsReference,
        time_to_receive: Duration,
        time_to_start_relaying: Duration,
        time_to_relay: Duration,
        latency: Duration,
    ) -> Self {
        Self {
            reference,
            time_to_receive,
            time_to_start_relaying,
            time_to_relay,
            latency,
        }
    }
}

impl Deltas for PollerDeltas {
    fn display(&self) {
        trace!(
            "reference: {:?}, time_to_receive: {:?}, time_to_start_relaying: {:?}, time_to_relay: {:?}, latency: {:?}",
            self.reference,
            self.time_to_receive,
            self.time_to_start_relaying,
            self.time_to_relay,
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

pub type IncomingCanisterMessageEvents = EventsImpl<IncomingCanisterMessageEventsMetrics>;

#[derive(Debug, Clone)]
pub struct IncomingCanisterMessageEventsMetrics {
    start_relaying_message: TimeableEvent,
    message_relayed: TimeableEvent,
}

impl IncomingCanisterMessageEventsMetrics {
    pub fn default() -> Self {
        Self {
            start_relaying_message: TimeableEvent::default(),
            message_relayed: TimeableEvent::default(),
        }
    }

    pub fn set_start_relaying_message(&mut self) {
        self.start_relaying_message.set_now();
    }

    pub fn set_message_relayed(&mut self) {
        self.message_relayed = TimeableEvent::now();
    }

    pub fn set_no_message_relayed(&mut self) {
        self.message_relayed = TimeableEvent::default();
    }
}

impl EventsMetrics for IncomingCanisterMessageEventsMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.message_relayed
    }

    fn compute_deltas(
        &self,
        reference: Option<&EventsReference>,
    ) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let time_to_relay = self
                .message_relayed
                .duration_since(&self.start_relaying_message)?;
            let latency = self.compute_latency()?;

            return Some(Box::new(IncomingCanisterMessageDeltas::new(
                reference.to_owned(),
                time_to_relay,
                latency,
            )));
        }
        None
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.message_relayed
            .duration_since(&self.start_relaying_message)
    }
}

#[derive(Debug)]
struct IncomingCanisterMessageDeltas {
    reference: EventsReference,
    time_to_relay: Duration,
    latency: Duration,
}

impl IncomingCanisterMessageDeltas {
    pub fn new(reference: EventsReference, time_to_relay: Duration, latency: Duration) -> Self {
        Self {
            reference,
            time_to_relay,
            latency,
        }
    }
}

impl Deltas for IncomingCanisterMessageDeltas {
    fn display(&self) {
        trace!(
            "reference: {:?}, time_to_relay: {:?}, latency: {:?}",
            self.reference,
            self.time_to_relay,
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

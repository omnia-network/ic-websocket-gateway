use crate::events_analyzer::{Deltas, EventsImpl, EventsMetrics, EventsReference, TimeableEvent};
use std::time::Duration;
use tracing::trace;

pub type PollerEvents = EventsImpl<PollerEventsMetrics>;

#[derive(Debug, Clone)]
pub struct PollerEventsMetrics {
    start_polling: TimeableEvent,
    received_messages: TimeableEvent,
    start_relaying_messages: TimeableEvent,
}

impl PollerEventsMetrics {
    pub fn default() -> Self {
        Self {
            start_polling: TimeableEvent::default(),
            received_messages: TimeableEvent::default(),
            start_relaying_messages: TimeableEvent::default(),
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
}

impl EventsMetrics for PollerEventsMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.received_messages
    }

    fn compute_deltas(&self, reference: Option<EventsReference>) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let time_to_receive = self.received_messages.duration_since(&self.start_polling)?;
            let time_to_start_relaying = self
                .start_relaying_messages
                .duration_since(&self.received_messages)?;
            let latency = self.compute_latency()?;

            return Some(Box::new(PollerDeltas::new(
                reference,
                time_to_receive,
                time_to_start_relaying,
                latency,
            )));
        }
        None
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.start_relaying_messages
            .duration_since(&self.start_polling)
    }
}

#[derive(Debug)]
struct PollerDeltas {
    reference: EventsReference,
    time_to_receive: Duration,
    time_to_start_relaying: Duration,
    latency: Duration,
}

impl PollerDeltas {
    pub fn new(
        reference: EventsReference,
        time_to_receive: Duration,
        time_to_start_relaying: Duration,
        latency: Duration,
    ) -> Self {
        Self {
            reference,
            time_to_receive,
            time_to_start_relaying,
            latency,
        }
    }
}

impl Deltas for PollerDeltas {
    fn display(&self) {
        trace!(
            "\ntime_to_receive: {:?}\ntime_to_start_relaying: {:?}\nlatency: {:?}",
            self.time_to_receive,
            self.time_to_start_relaying,
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

pub type IcWsEstablishmentNotificationEvents =
    EventsImpl<IcWsEstablishmentNotificationEventsMetrics>;

#[derive(Debug, Clone)]
pub struct IcWsEstablishmentNotificationEventsMetrics {
    received_client_channel: TimeableEvent,
    sent_client_notification: TimeableEvent,
}

impl IcWsEstablishmentNotificationEventsMetrics {
    pub fn default() -> Self {
        Self {
            received_client_channel: TimeableEvent::default(),
            sent_client_notification: TimeableEvent::default(),
        }
    }

    pub fn set_received_client_channel(&mut self) {
        self.received_client_channel.set_now();
    }

    pub fn set_sent_client_notification(&mut self) {
        self.sent_client_notification.set_now();
    }
}

impl EventsMetrics for IcWsEstablishmentNotificationEventsMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.sent_client_notification
    }

    fn compute_deltas(&self, reference: Option<EventsReference>) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let time_to_notify_client = self
                .sent_client_notification
                .duration_since(&self.received_client_channel)?;
            let latency = self.compute_latency()?;

            return Some(Box::new(IcWsEstablishmentNotificationDeltas::new(
                reference,
                time_to_notify_client,
                latency,
            )));
        }
        None
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.sent_client_notification
            .duration_since(&self.received_client_channel)
    }
}

#[derive(Debug)]
struct IcWsEstablishmentNotificationDeltas {
    reference: EventsReference,
    time_to_notify_client: Duration,
    latency: Duration,
}

impl IcWsEstablishmentNotificationDeltas {
    pub fn new(
        reference: EventsReference,
        time_to_notify_client: Duration,
        latency: Duration,
    ) -> Self {
        Self {
            reference,
            time_to_notify_client,
            latency,
        }
    }
}

impl Deltas for IcWsEstablishmentNotificationDeltas {
    fn display(&self) {
        trace!(
            "\ntime_to_notify_client: {:?}\nlatency: {:?}",
            self.time_to_notify_client,
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

    fn compute_deltas(&self, reference: Option<EventsReference>) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let time_to_relay = self
                .message_relayed
                .duration_since(&self.start_relaying_message)?;
            let latency = self.compute_latency()?;

            return Some(Box::new(IncomingCanisterMessageDeltas::new(
                reference,
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
            "\nreference: {:?}\ntime_to_relay: {:?}\nlatency: {:?}",
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

use crate::events_analyzer::{Deltas, EventsImpl, EventsMetrics, EventsReference, TimeableEvent};
use std::time::Duration;
use tracing::trace;

pub type ConnectionEstablishmentEvents = EventsImpl<ConnectionEstablishmentEventsMetrics>;

#[derive(Debug, Clone)]
pub struct ConnectionEstablishmentEventsMetrics {
    received_client_session: TimeableEvent,
    started_new_poller: TimeableEvent,
    sent_client_channel_to_poller: TimeableEvent,
}

impl ConnectionEstablishmentEventsMetrics {
    pub fn default() -> Self {
        Self {
            received_client_session: TimeableEvent::default(),
            started_new_poller: TimeableEvent::default(),
            sent_client_channel_to_poller: TimeableEvent::default(),
        }
    }

    pub fn set_received_client_session(&mut self) {
        self.received_client_session.set_now();
    }

    pub fn set_started_new_poller(&mut self) {
        self.started_new_poller.set_now();
    }

    pub fn set_sent_client_channel_to_poller(&mut self) {
        self.sent_client_channel_to_poller.set_now();
    }
}

impl EventsMetrics for ConnectionEstablishmentEventsMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.sent_client_channel_to_poller
    }

    fn compute_deltas(
        &self,
        reference: Option<&EventsReference>,
    ) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let time_to_start_poller = self
                .started_new_poller
                .duration_since(&self.received_client_session)
                // if the poller has already been started, we consider this latency as zero
                .unwrap_or(Duration::from_millis(0));
            let time_to_send_client_channel = self
                .sent_client_channel_to_poller
                .duration_since(&self.started_new_poller)
                // if the poller has already been started, we consider the latency since the client was added to the gateway state
                .unwrap_or(
                    self.sent_client_channel_to_poller
                        .duration_since(&self.received_client_session)?,
                );
            let latency = self.compute_latency()?;

            return Some(Box::new(ConnectionEstablishmentDeltas::new(
                reference.to_owned(),
                time_to_start_poller,
                time_to_send_client_channel,
                latency,
            )));
        }
        None
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.sent_client_channel_to_poller
            .duration_since(&self.received_client_session)
    }
}

#[derive(Debug)]
struct ConnectionEstablishmentDeltas {
    reference: EventsReference,
    time_to_start_poller: Duration,
    time_to_send_client_channel: Duration,
    latency: Duration,
}

impl ConnectionEstablishmentDeltas {
    pub fn new(
        reference: EventsReference,
        time_to_start_poller: Duration,
        time_to_send_client_channel: Duration,
        latency: Duration,
    ) -> Self {
        Self {
            reference,
            time_to_start_poller,
            time_to_send_client_channel,
            latency,
        }
    }
}

impl Deltas for ConnectionEstablishmentDeltas {
    fn display(&self) {
        trace!(
            "reference: {:?}, time_to_start_poller: {:?}, time_to_send_client_channel: {:?}, latency: {:?}",
            self.reference,
            self.time_to_start_poller,
            self.time_to_send_client_channel,
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
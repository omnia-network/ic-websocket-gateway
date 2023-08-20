use crate::events_analyzer::{Deltas, EventsImpl, EventsMetrics, EventsReference, TimeableEvent};
use std::time::Duration;
use tracing::debug;

pub type ConnectionEstablishmentEvents = EventsImpl<ConnectionEstablishmentEventsMetrics>;

#[derive(Debug, Clone)]
pub struct ConnectionEstablishmentEventsMetrics {
    added_client_to_state: TimeableEvent,
    started_new_poller: TimeableEvent,
    sent_client_channel_to_poller: TimeableEvent,
    sent_client_key_to_canister: TimeableEvent,
}

impl ConnectionEstablishmentEventsMetrics {
    pub fn default() -> Self {
        Self {
            added_client_to_state: TimeableEvent::default(),
            started_new_poller: TimeableEvent::default(),
            sent_client_channel_to_poller: TimeableEvent::default(),
            sent_client_key_to_canister: TimeableEvent::default(),
        }
    }

    pub fn set_added_client_to_state(&mut self) {
        self.added_client_to_state.set_now();
    }

    pub fn set_started_new_poller(&mut self) {
        self.started_new_poller.set_now();
    }

    pub fn set_sent_client_channel_to_poller(&mut self) {
        self.sent_client_channel_to_poller.set_now();
    }

    pub fn set_sent_client_key_to_canister(&mut self) {
        self.sent_client_key_to_canister.set_now();
    }
}

impl EventsMetrics for ConnectionEstablishmentEventsMetrics {
    fn get_value_for_interval(&self) -> &TimeableEvent {
        &self.sent_client_channel_to_poller
    }

    fn compute_deltas(&self, reference: Option<EventsReference>) -> Option<Box<dyn Deltas + Send>> {
        if let Some(reference) = reference {
            let time_to_start_poller = self
                .started_new_poller
                .duration_since(&self.added_client_to_state)
                // if the poller has already been started, we consider this latency as zero
                .unwrap_or(Duration::from_millis(0));
            let time_to_send_client_channel = {
                if self.started_new_poller.is_set() {
                    self.sent_client_channel_to_poller
                        .duration_since(&self.started_new_poller)?
                } else {
                    self.sent_client_channel_to_poller
                        .duration_since(&self.sent_client_channel_to_poller)?
                }
            };
            let time_to_send_client_key = self
                .sent_client_key_to_canister
                .duration_since(&self.added_client_to_state)?;
            let latency = self.compute_latency()?;

            return Some(Box::new(ConnectionEstablishmentDeltas::new(
                reference,
                time_to_start_poller,
                time_to_send_client_channel,
                time_to_send_client_key,
                latency,
            )));
        }
        None
    }

    fn compute_latency(&self) -> Option<Duration> {
        self.sent_client_key_to_canister
            .duration_since(&self.added_client_to_state)
    }
}

#[derive(Debug)]
struct ConnectionEstablishmentDeltas {
    reference: EventsReference,
    time_to_start_poller: Duration,
    time_to_send_client_channel: Duration,
    time_to_send_client_key: Duration,
    latency: Duration,
}

impl ConnectionEstablishmentDeltas {
    pub fn new(
        reference: EventsReference,
        time_to_start_poller: Duration,
        time_to_send_client_channel: Duration,
        time_to_send_client_key: Duration,
        latency: Duration,
    ) -> Self {
        Self {
            reference,
            time_to_start_poller,
            time_to_send_client_channel,
            time_to_send_client_key,
            latency,
        }
    }
}

impl Deltas for ConnectionEstablishmentDeltas {
    fn display(&self) {
        debug!(
            "\nreference: {:?}\ntime_to_start_poller: {:?}\ntime_to_send_client_channel: {:?}\ntime_to_send_client_key: {:?}\nlatency: {:?}",
            self.reference, self.time_to_start_poller, self.time_to_send_client_channel, self.time_to_send_client_key, self.latency
        );
    }

    fn get_reference(&self) -> &EventsReference {
        &self.reference
    }

    fn get_latency(&self) -> Duration {
        self.latency
    }
}

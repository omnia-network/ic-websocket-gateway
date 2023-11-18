use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use candid::Principal;
use tokio::sync::{mpsc::Sender, RwLock};
use tracing::{debug, error, span, trace, warn, Instrument, Level, Span};

use crate::{
    canister_methods::{CanisterOutputCertifiedMessages, CanisterToClientMessage, ClientKey},
    canister_poller::IcWsConnectionUpdate,
    events_analyzer::{
        Events, EventsCollectionType, EventsImpl, EventsReference, MessageReference,
    },
    metrics::canister_poller_metrics::{
        IncomingCanisterMessageEvents, IncomingCanisterMessageEventsMetrics,
    },
};

/// number of clients registered in the CDK
pub static CLIENTS_REGISTERED_IN_CDK: AtomicUsize = AtomicUsize::new(0);

pub struct MessagesDemux {
    canister_id: Principal,
    /// channels used to communicate with the connection handler task of the client identified by the client key
    client_channels: HashMap<ClientKey, (Sender<IcWsConnectionUpdate>, Span)>,
    clients_message_queues: HashMap<
        ClientKey,
        Vec<(
            CanisterToClientMessage,
            EventsImpl<IncomingCanisterMessageEventsMetrics>,
        )>,
    >,
    recently_removed_clients: HashSet<ClientKey>,
    analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
}

impl MessagesDemux {
    pub fn new(
        analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
        canister_id: Principal,
    ) -> Self {
        // queues where the poller temporarily stores messages received from the canister before a client is registered
        // this is needed because the poller might get a message for a client which is not yet regiatered in the poller
        let clients_message_queues: HashMap<
            ClientKey,
            Vec<(
                CanisterToClientMessage,
                EventsImpl<IncomingCanisterMessageEventsMetrics>,
            )>,
        > = HashMap::new();

        Self {
            canister_id,
            client_channels: HashMap::new(),
            clients_message_queues,
            recently_removed_clients: HashSet::new(),
            analyzer_channel_tx,
        }
    }

    pub fn remove_client_state(&mut self, client_key: &ClientKey) {
        self.remove_client_channel(client_key);
        self.remove_client_message_queue(client_key);
        // TODO: forget client keys after a while
        self.recently_removed_clients.insert(client_key.to_owned());
    }

    pub fn client_channels(&self) -> &HashMap<ClientKey, (Sender<IcWsConnectionUpdate>, Span)> {
        &self.client_channels
    }

    pub fn add_client_channel(
        &mut self,
        client_key: ClientKey,
        client_channel: Sender<IcWsConnectionUpdate>,
        parent_span: Span,
    ) {
        parent_span.in_scope(|| {
            debug!("Added new channel to poller");
        });
        CLIENTS_REGISTERED_IN_CDK.fetch_add(1, Ordering::SeqCst);
        self.client_channels
            .insert(client_key.clone(), (client_channel, parent_span));
    }

    fn remove_client_channel(&mut self, client_key: &ClientKey) {
        debug!(
            "Removed client channel from poller for client {:?}",
            client_key
        );
        self.client_channels.remove(client_key);
    }

    pub fn count_registered_clients(&self) -> usize {
        let count = CLIENTS_REGISTERED_IN_CDK.load(Ordering::SeqCst);
        trace!("{} clients connected to poller", count);
        count
    }

    pub fn add_message_to_client_queue(
        &mut self,
        client_key: &ClientKey,
        message: (
            CanisterToClientMessage,
            EventsImpl<IncomingCanisterMessageEventsMetrics>,
        ),
    ) {
        debug!(
            "Added message queue from poller for client {:?}",
            client_key
        );
        if let Some(message_queue) = self.clients_message_queues.get_mut(&client_key) {
            message_queue.push(message);
        } else {
            self.clients_message_queues
                .insert(client_key.clone(), vec![message]);
        }
    }

    fn remove_client_message_queue(&mut self, client_key: &ClientKey) {
        debug!(
            "Removed message queue from poller for client {:?}",
            client_key
        );
        self.clients_message_queues.remove(client_key);
    }

    #[cfg(test)]
    pub fn count_clients_message_queues(&self) -> usize {
        self.clients_message_queues.len()
    }

    pub async fn process_queues(&mut self) {
        let mut to_be_relayed = Vec::new();
        self.clients_message_queues
            .retain(|client_key, message_queue| {
                if let Some(client_channel_tx) = self.client_channels.get(&client_key) {
                    // once a client channel is received, messages for that client will not be put in the queue anymore (until that client disconnects)
                    // thus the respective queue does not need to be retained
                    // relay all the messages previously received for the corresponding client
                    to_be_relayed.push((client_channel_tx.to_owned(), message_queue.to_owned()));
                    return false;
                }
                // if the client channel has not been received yet, keep the messages in the queue
                true
            });
        // the tasks must be awaited so that messages in queue are relayed before newly polled messages
        for ((client_channel_tx, parent_span), message_queue) in to_be_relayed {
            for (canister_to_client_message, incoming_canister_message_events) in message_queue {
                let relay_message_span = span!(parent: &parent_span, Level::TRACE, "relay_message", message_key = canister_to_client_message.key);
                relay_message_span.in_scope(|| {
                    warn!("Processing message from queue");
                });
                self.relay_message(
                    canister_to_client_message,
                    &client_channel_tx,
                    incoming_canister_message_events,
                )
                .instrument(relay_message_span)
                .await;
            }
            drop(parent_span);
        }
    }

    pub async fn relay_messages(
        &mut self,
        msgs: CanisterOutputCertifiedMessages,
        message_nonce: Arc<RwLock<u64>>,
        polling_iteration_span: Span,
    ) -> Result<(), String> {
        for canister_output_message in msgs.messages {
            let canister_to_client_message = CanisterToClientMessage {
                key: canister_output_message.key,
                content: canister_output_message.content,
                cert: msgs.cert.clone(),
                tree: msgs.tree.clone(),
            };
            let mut incoming_canister_message_events = IncomingCanisterMessageEvents::new(
                None,
                EventsCollectionType::CanisterMessage,
                IncomingCanisterMessageEventsMetrics::default(),
            );
            incoming_canister_message_events
                .metrics
                .set_start_relaying_message();

            let last_message_nonce = get_nonce_from_message(&canister_to_client_message.key)?;
            let message_key = MessageReference::new(self.canister_id, last_message_nonce.clone());
            incoming_canister_message_events.reference =
                Some(EventsReference::MessageReference(message_key));
            match self
                .client_channels
                .get(&canister_output_message.client_key)
            {
                Some((client_channel_tx, parent_span)) => {
                    let relay_message_span = span!(parent: parent_span, Level::TRACE, "relay_message", message_key = canister_to_client_message.key);
                    relay_message_span.follows_from(&polling_iteration_span);
                    relay_message_span.in_scope(|| trace!("Received message from canister",));
                    self.relay_message(
                        canister_to_client_message,
                        client_channel_tx,
                        incoming_canister_message_events,
                    )
                    .instrument(relay_message_span)
                    .await;
                },
                None => {
                    // distinguish the case in which there is no client channel because the client's session hasn't been registered (yet)
                    // from the case in which the client has just disconnected (and its client channel removed before the polling returns new messages fot that client)
                    if !self
                        .recently_removed_clients
                        .contains(&canister_output_message.client_key)
                    {
                        // if the client wasn't previously removed, it hasn't connected yet and therefore the message should be added to the queue
                        warn!("Connection to client with principal: {:?} not opened yet. Adding message with key: {:?} to queue", canister_output_message.client_key, canister_to_client_message.key);
                        self.add_message_to_client_queue(
                            &canister_output_message.client_key,
                            (canister_to_client_message, incoming_canister_message_events),
                        )
                    }
                },
            }
            // TODO: check without panicking
            // assert_eq!(*message_nonce, last_message_nonce); // check that messages are relayed in increasing order

            *message_nonce.write().await = last_message_nonce + 1;
        }
        drop(polling_iteration_span);
        Ok(())
    }

    pub async fn relay_message(
        &self,
        canister_to_client_message: CanisterToClientMessage,
        client_channel_tx: &Sender<IcWsConnectionUpdate>,
        mut incoming_canister_message_events: EventsImpl<IncomingCanisterMessageEventsMetrics>,
    ) {
        if let Err(e) = client_channel_tx
            .send(IcWsConnectionUpdate::Message(canister_to_client_message))
            .await
        {
            error!("Client's thread terminated: {}", e);
            incoming_canister_message_events
                .metrics
                .set_no_message_relayed();
        } else {
            incoming_canister_message_events
                .metrics
                .set_message_relayed();
            trace!("Message relayed to connection handler");
        }
        self.analyzer_channel_tx
            .send(Box::new(incoming_canister_message_events))
            .await
            .expect("analyzer's side of the channel dropped");
    }
}

pub fn get_nonce_from_message(key: &String) -> Result<u64, String> {
    if let Some(message_nonce_str) = key.split('_').last() {
        let message_nonce = message_nonce_str
            .parse()
            .map_err(|e| format!("Could not parse nonce. Error: {:?}", e))?;
        return Ok(message_nonce);
    }
    Err(String::from(
        "Key in canister message is not formatted correctly",
    ))
}

use std::{collections::HashMap, sync::Arc};

use tokio::sync::{mpsc::Sender, RwLock};
use tracing::{debug, error, trace, warn};

use crate::{
    canister_methods::{CanisterOutputCertifiedMessages, CanisterToClientMessage, ClientKey},
    canister_poller::IcWsConnectionUpdate,
    events_analyzer::{Events, EventsCollectionType, EventsImpl, EventsReference},
    metrics::canister_poller_metrics::{
        IncomingCanisterMessageEvents, IncomingCanisterMessageEventsMetrics,
    },
};

pub struct MessagesDemux {
    /// channels used to communicate with the connection handler task of the client identified by the client key
    client_channels: HashMap<ClientKey, Sender<IcWsConnectionUpdate>>,
    analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
}

impl MessagesDemux {
    pub fn new(analyzer_channel_tx: Sender<Box<dyn Events + Send>>) -> Self {
        Self {
            client_channels: HashMap::new(),
            analyzer_channel_tx,
        }
    }

    pub fn client_channels(&self) -> Vec<&Sender<IcWsConnectionUpdate>> {
        self.client_channels.values().collect()
    }

    pub fn add_client_channel(
        &mut self,
        client_key: ClientKey,
        client_channel: Sender<IcWsConnectionUpdate>,
    ) {
        debug!("Added new channel to poller for client: {:?}", client_key);
        self.client_channels
            .insert(client_key.clone(), client_channel);
    }

    pub fn remove_client_channel(&mut self, client_key: &ClientKey) {
        debug!(
            "Removed client channel from poller for client {:?}",
            client_key
        );
        self.client_channels.remove(client_key);
    }

    pub fn count_client_channels(&self) -> usize {
        let count = self.client_channels.len();
        debug!("{} clients connected to poller", count);
        count
    }

    pub async fn process_queues(
        &self,
        clients_message_queues: &mut HashMap<
            ClientKey,
            Vec<(
                CanisterToClientMessage,
                EventsImpl<IncomingCanisterMessageEventsMetrics>,
            )>,
        >,
    ) {
        let mut to_be_relayed = Vec::new();
        clients_message_queues.retain(|client_key, message_queue| {
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
        for (client_channel_tx, message_queue) in to_be_relayed {
            for (canister_to_client_message, incoming_canister_message_events) in message_queue {
                warn!(
                    "Processing message with key: {:?} from queue",
                    canister_to_client_message.key
                );
                self.relay_message(
                    canister_to_client_message,
                    &client_channel_tx,
                    incoming_canister_message_events,
                )
                .await;
            }
        }
    }

    pub async fn relay_messages(
        &self,
        msgs: CanisterOutputCertifiedMessages,
        clients_message_queues: &mut HashMap<
            ClientKey,
            Vec<(
                CanisterToClientMessage,
                EventsImpl<IncomingCanisterMessageEventsMetrics>,
            )>,
        >,
        message_nonce: Arc<RwLock<u64>>,
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
            incoming_canister_message_events.reference =
                Some(EventsReference::MessageNonce(last_message_nonce));
            match self
                .client_channels
                .get(&canister_output_message.client_key)
            {
                Some(client_channel_tx) => {
                    trace!(
                        "Received message with key: {:?} from canister",
                        canister_to_client_message.key
                    );
                    self.relay_message(
                        canister_to_client_message,
                        client_channel_tx,
                        incoming_canister_message_events,
                    )
                    .await;

                    // TODO: check without panicking
                    // assert_eq!(*message_nonce, last_message_nonce); // check that messages are relayed in increasing order

                    *message_nonce.write().await = last_message_nonce + 1;
                },
                None => {
                    // TODO: we should distinguish the case in which there is no client channel because the client's state hasn't been registered (yet)
                    //       from the case in which the client has just disconnected (and its client channel removed before the polling returns new messages fot that client)
                    warn!("Connection to client with principal: {:?} not opened yet. Adding message with key: {:?} to queue", canister_to_client_message.key, canister_output_message.client_key);
                    if let Some(message_queue) =
                        clients_message_queues.get_mut(&canister_output_message.client_key)
                    {
                        message_queue
                            .push((canister_to_client_message, incoming_canister_message_events));
                    } else {
                        clients_message_queues.insert(
                            canister_output_message.client_key,
                            vec![(canister_to_client_message, incoming_canister_message_events)],
                        );
                    }
                },
            }
        }
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

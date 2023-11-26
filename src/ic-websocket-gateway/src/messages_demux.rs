// use std::{
//     collections::{HashMap, HashSet},
//     sync::{
//         atomic::{AtomicUsize, Ordering},
//         Arc,
//     },
// };

// use candid::Principal;
// use tokio::sync::{mpsc::Sender, RwLock};
// use tracing::{debug, error, span, trace, warn, Id, Instrument, Level, Span};

// use crate::{
//     canister_methods::{CanisterOutputCertifiedMessages, CanisterToClientMessage, ClientKey},
//     canister_poller::IcWsConnectionUpdate,
//     events_analyzer::{
//         Events, EventsCollectionType, EventsImpl, EventsReference, MessageReference,
//     },
//     metrics::canister_poller_metrics::{
//         IncomingCanisterMessageEvents, IncomingCanisterMessageEventsMetrics,
//     },
// };

// /// number of clients registered in the CDK
// pub static CLIENTS_REGISTERED_IN_CDK: AtomicUsize = AtomicUsize::new(0);

// pub struct MessagesDemux {
//     canister_id: Principal,
//     /// channels used to communicate with the connection handler task of the client identified by the client key
//     client_channels: HashMap<ClientKey, (Sender<IcWsConnectionUpdate>, Span)>,
//     clients_message_queues: HashMap<
//         ClientKey,
//         Vec<(
//             CanisterToClientMessage,
//             EventsImpl<IncomingCanisterMessageEventsMetrics>,
//         )>,
//     >,
//     recently_removed_clients: HashSet<ClientKey>,
//     analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
// }

// impl MessagesDemux {
//     pub fn new(
//         analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
//         canister_id: Principal,
//     ) -> Self {
//         // queues where the poller temporarily stores messages received from the canister before a client is registered
//         // this is needed because the poller might get a message for a client which is not yet regiatered in the poller
//         let clients_message_queues: HashMap<
//             ClientKey,
//             Vec<(
//                 CanisterToClientMessage,
//                 EventsImpl<IncomingCanisterMessageEventsMetrics>,
//             )>,
//         > = HashMap::new();

//         Self {
//             canister_id,
//             client_channels: HashMap::new(),
//             clients_message_queues,
//             recently_removed_clients: HashSet::new(),
//             analyzer_channel_tx,
//         }
//     }

//     pub fn remove_client_state(&mut self, client_key: &ClientKey, span: Span) {
//         let _guard = span.entered();
//         self.remove_client_channel(client_key, Span::current());
//         self.remove_client_message_queue(client_key, Span::current());
//         // TODO: forget client keys after a while
//         self.recently_removed_clients.insert(client_key.to_owned());
//     }

//     pub fn client_channels(&self) -> &HashMap<ClientKey, (Sender<IcWsConnectionUpdate>, Span)> {
//         &self.client_channels
//     }

//     pub fn add_client_channel(
//         &mut self,
//         client_key: ClientKey,
//         client_channel: Sender<IcWsConnectionUpdate>,
//         client_connection_span: Span,
//     ) {
//         client_connection_span.in_scope(|| {
//             debug!("Added new channel to poller");
//         });
//         CLIENTS_REGISTERED_IN_CDK.fetch_add(1, Ordering::SeqCst);
//         self.client_channels
//             .insert(client_key.clone(), (client_channel, client_connection_span));
//     }

//     fn remove_client_channel(&mut self, client_key: &ClientKey, span: Span) {
//         let _guard = span.entered();
//         debug!("Removed client channel from poller");
//         self.client_channels.remove(client_key);
//     }

//     pub fn count_registered_clients(&self) -> usize {
//         let count = CLIENTS_REGISTERED_IN_CDK.load(Ordering::SeqCst);
//         trace!("{} clients connected to poller", count);
//         count
//     }

//     pub fn add_message_to_client_queue(
//         &mut self,
//         client_key: &ClientKey,
//         message: (
//             CanisterToClientMessage,
//             EventsImpl<IncomingCanisterMessageEventsMetrics>,
//         ),
//     ) {
//         debug!("Added message queue from poller");
//         if let Some(message_queue) = self.clients_message_queues.get_mut(&client_key) {
//             message_queue.push(message);
//         } else {
//             self.clients_message_queues
//                 .insert(client_key.clone(), vec![message]);
//         }
//     }

//     fn remove_client_message_queue(&mut self, client_key: &ClientKey, span: Span) {
//         let _guard = span.entered();
//         debug!("Removed message queue from poller");
//         self.clients_message_queues.remove(client_key);
//     }

//     #[cfg(test)]
//     pub fn count_clients_message_queues(&self) -> usize {
//         self.clients_message_queues.len()
//     }

//     pub async fn process_queues(&mut self, polling_iteration_span_id: Option<Id>) {
//         let mut to_be_relayed = Vec::new();
//         self.clients_message_queues
//             .retain(|client_key, message_queue| {
//                 if let Some(client_channel_tx) = self.client_channels.get(&client_key) {
//                     // once a client channel is received, messages for that client will not be put in the queue anymore (until that client disconnects)
//                     // thus the respective queue does not need to be retained
//                     // relay all the messages previously received for the corresponding client
//                     to_be_relayed
//                         .push((client_channel_tx.to_owned(), message_queue.to_owned()));
//                     return false;
//                 }
//                 // if the client channel has not been received yet, keep the messages in the queue
//                 true
//             });
//         // the tasks must be awaited so that messages in queue are relayed before newly polled messages
//         for ((client_channel_tx, parent_span), message_queue) in to_be_relayed {
//             for (canister_to_client_message, incoming_canister_message_events) in message_queue {
//                 let canister_message_span = span!(parent: &parent_span, Level::TRACE, "canister_message", message_key = canister_to_client_message.key);
//                 canister_message_span.follows_from(polling_iteration_span_id.clone());
//                 canister_message_span.in_scope(|| {
//                     warn!("Processing message from queue");
//                 });
//                 self.relay_message(
//                     canister_to_client_message,
//                     &client_channel_tx,
//                     incoming_canister_message_events,
//                 )
//                 .instrument(canister_message_span)
//                 .await;
//             }
//             drop(parent_span);
//         }
//     }
// }

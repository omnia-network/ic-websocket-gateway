// #[cfg(test)]
// mod tests {
//     use std::sync::Arc;

//     use crate::canister_methods::{
//         CanisterAckMessageContent, CanisterOpenMessageContent, CanisterOutputCertifiedMessages,
//         CanisterOutputMessage, CanisterServiceMessage, CanisterToClientMessage, ClientKey,
//         WebsocketMessage,
//     };
//     use crate::canister_poller::{
//         filter_canister_messages, filter_messages_of_first_polling_iteration, IcWsConnectionUpdate,
//     };
//     use crate::events_analyzer::{Events, EventsCollectionType};
//     use crate::messages_demux::MessagesDemux;
//     use crate::metrics::canister_poller_metrics::{
//         IncomingCanisterMessageEvents, IncomingCanisterMessageEventsMetrics,
//     };
//     use candid::{encode_one, Principal};
//     use serde::Serialize;
//     use serde_cbor::{from_slice, Serializer};
//     use tokio::sync::mpsc::{self, Receiver, Sender};
//     use tokio::sync::RwLock;
//     use tracing::Span;

//     fn init_poller() -> (
//         Sender<IcWsConnectionUpdate>,
//         Receiver<IcWsConnectionUpdate>,
//         Sender<Box<dyn Events + Send>>,
//         Receiver<Box<dyn Events + Send>>,
//     ) {
//         let (message_for_client_tx, message_for_client_rx): (
//             Sender<IcWsConnectionUpdate>,
//             Receiver<IcWsConnectionUpdate>,
//         ) = mpsc::channel(100);

//         let (analyzer_channel_tx, analyzer_channel_rx) = mpsc::channel(100);

//         (
//             message_for_client_tx,
//             message_for_client_rx,
//             analyzer_channel_tx,
//             analyzer_channel_rx,
//         )
//     }

//     fn init_messages_demux(analyzer_channel_tx: Sender<Box<dyn Events + Send>>) -> MessagesDemux {
//         MessagesDemux::new(analyzer_channel_tx, Principal::anonymous())
//     }

//     fn cbor_serialize<T: Serialize>(m: T) -> Vec<u8> {
//         let mut bytes = Vec::new();
//         let mut serializer = Serializer::new(&mut bytes);
//         serializer.self_describe().unwrap();
//         m.serialize(&mut serializer).unwrap();
//         bytes
//     }

//     fn mock_open_message(client_key: &ClientKey) -> CanisterServiceMessage {
//         CanisterServiceMessage::OpenMessage(CanisterOpenMessageContent {
//             client_key: client_key.clone(),
//         })
//     }

//     fn mock_ack_message() -> CanisterServiceMessage {
//         CanisterServiceMessage::AckMessage(CanisterAckMessageContent {
//             last_incoming_sequence_num: 0,
//         })
//     }

//     fn mock_websocket_service_message(
//         content: CanisterServiceMessage,
//         client_key: &ClientKey,
//         sequence_num: u64,
//     ) -> WebsocketMessage {
//         WebsocketMessage {
//             client_key: client_key.clone(),
//             sequence_num,
//             timestamp: 0,
//             is_service_message: true,
//             content: encode_one(content).unwrap(),
//         }
//     }

//     fn mock_websocket_message(client_key: &ClientKey, sequence_num: u64) -> WebsocketMessage {
//         WebsocketMessage {
//             client_key: client_key.clone(),
//             sequence_num,
//             timestamp: 0,
//             is_service_message: false,
//             content: vec![],
//         }
//     }

//     fn mock_canister_output_message(
//         content: WebsocketMessage,
//         client_key: &ClientKey,
//     ) -> CanisterOutputMessage {
//         CanisterOutputMessage {
//             client_key: client_key.clone(),
//             key: String::from("gateway_uid_0"),
//             content: cbor_serialize(content),
//         }
//     }

//     fn canister_open_message(client_key: &ClientKey, sequence_num: u64) -> CanisterOutputMessage {
//         let open_message = mock_open_message(client_key);
//         let websocket_service_message =
//             mock_websocket_service_message(open_message, client_key, sequence_num);
//         mock_canister_output_message(websocket_service_message, client_key)
//     }

//     fn canister_ack_message(client_key: &ClientKey, sequence_num: u64) -> CanisterOutputMessage {
//         let ack_message = mock_ack_message();
//         let websocket_service_message =
//             mock_websocket_service_message(ack_message, client_key, sequence_num);
//         mock_canister_output_message(websocket_service_message, client_key)
//     }

//     fn canister_output_message(client_key: &ClientKey, sequence_num: u64) -> CanisterOutputMessage {
//         let websocket_message = mock_websocket_message(client_key, sequence_num);
//         mock_canister_output_message(websocket_message, client_key)
//     }

//     fn mock_messages_to_be_filtered() -> Vec<CanisterOutputMessage> {
//         let old_client_key = ClientKey::new(Principal::from_text("aaaaa-aa").unwrap(), 0);
//         let reconnecting_client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );
//         let new_client_key = ClientKey::new(
//             Principal::from_text("ygoe7-xpj6n-24gsd-zksfw-2mywm-xfyop-yvlsp-ctlwa-753xv-wz6rk-uae")
//                 .unwrap(),
//             0,
//         );

//         let mut messages = Vec::new();

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_output_message(&old_client_key, 10);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_open_message(&reconnecting_client_key, 0);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_ack_message(&old_client_key, 11);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_output_message(&reconnecting_client_key, 1);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_output_message(&reconnecting_client_key, 2);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_output_message(&reconnecting_client_key, 3);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_ack_message(&reconnecting_client_key, 4);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_output_message(&old_client_key, 12);
//         messages.push(canister_message);

//         // the gateway reboots and therefore all the previous connections are closed
//         // client 2chl6-4hpzw-vqaaa-aaaaa-c reconnects
//         // new client ygoe7-xpj6n-24gsd-zksfw-2mywm-xfyop-yvlsp-ctlwa-753xv-wz6rk-uae connects

//         // this message should not be filtered out as it is the open message sent by the first client that (re)connects after the gateway reboots
//         let canister_message = canister_open_message(&reconnecting_client_key, 0);
//         messages.push(canister_message);

//         // this message should not be filtered out as it was sent after the gateway rebooted
//         let canister_message = canister_output_message(&reconnecting_client_key, 1);
//         messages.push(canister_message);

//         // this message should not be filtered out as it was sent after the gateway rebooted
//         let canister_message = canister_output_message(&reconnecting_client_key, 2);
//         messages.push(canister_message);

//         // this message should not be filtered out as it was sent after the gateway rebooted
//         let canister_message = canister_open_message(&new_client_key, 0);
//         messages.push(canister_message);

//         // this message should not be filtered out as it was sent after the gateway rebooted
//         let canister_message = canister_output_message(&new_client_key, 1);
//         messages.push(canister_message);

//         messages
//     }

//     fn mock_all_old_messages_to_be_filtered() -> Vec<CanisterOutputMessage> {
//         let old_client_key = ClientKey::new(Principal::from_text("aaaaa-aa").unwrap(), 0);
//         let reconnecting_client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );

//         let mut messages = Vec::new();

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_output_message(&old_client_key, 10);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_ack_message(&old_client_key, 11);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_output_message(&reconnecting_client_key, 1);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_output_message(&reconnecting_client_key, 2);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_output_message(&reconnecting_client_key, 3);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_ack_message(&reconnecting_client_key, 4);
//         messages.push(canister_message);

//         // this message should be filtered out as it was sent before the gateway rebooted
//         let canister_message = canister_output_message(&old_client_key, 12);
//         messages.push(canister_message);

//         // the gateway reboots and therefore all the previous connections are closed
//         // client 2chl6-4hpzw-vqaaa-aaaaa-c will reconnect but its open message is not ready for this polling iteration

//         // all messages should therefore be filtered out
//         messages
//     }

//     fn mock_ordered_messages(
//         client_key: &ClientKey,
//         start_sequence_number: u64,
//     ) -> Vec<CanisterOutputMessage> {
//         let mut sequence_number = start_sequence_number;

//         let mut messages = Vec::new();
//         while sequence_number < 10 {
//             let canister_message = canister_output_message(&client_key, sequence_number);
//             messages.push(canister_message);
//             sequence_number += 1;
//         }
//         messages
//     }

//     fn mock_incoming_canister_message_events() -> IncomingCanisterMessageEvents {
//         IncomingCanisterMessageEvents::new(
//             None,
//             EventsCollectionType::CanisterMessage,
//             IncomingCanisterMessageEventsMetrics::default(),
//         )
//     }

//     #[tokio::test()]
//     /// Simulates the case in which polled messages are filtered before being relayed.
//     /// The messages that are not fltered out should be relayed in the same order as when polled.
//     async fn should_return_filtered_messages_in_order() {
//         let client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );

//         let mut messages = mock_messages_to_be_filtered();
//         filter_messages_of_first_polling_iteration(&mut messages, client_key.clone());
//         assert_eq!(messages.len(), 5);

//         let mut expected_sequence_number = 0;
//         for canister_output_message in messages {
//             let websocket_message: WebsocketMessage = from_slice(&canister_output_message.content)
//                 .expect("content of canister_output_message is not of type WebsocketMessage");
//             if websocket_message.client_key == client_key {
//                 assert_eq!(websocket_message.sequence_num, expected_sequence_number);
//                 expected_sequence_number += 1;
//             }
//         }
//         assert_eq!(expected_sequence_number, 3);
//     }

//     #[tokio::test()]
//     /// Simulates the case in which polled messages are all from before the gateway rebooted.
//     /// The messages that are not fltered out should be relayed in the same order as when polled.
//     async fn should_filter_out_all_messages() {
//         let client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );

//         let mut messages = mock_all_old_messages_to_be_filtered();
//         filter_messages_of_first_polling_iteration(&mut messages, client_key);
//         assert_eq!(messages.len(), 0);
//     }

//     #[tokio::test()]
//     /// Simulates the case in which the poller starts and the canister's queue contains some old messages.
//     /// Relays only open messages for the connected clients.
//     async fn should_process_canister_messages() {
//         let (
//             message_for_client_tx,
//             mut message_for_client_rx,
//             analyzer_channel_tx,
//             // the following have to be returned in order not to drop them
//             _analyzer_channel_rx,
//         ) = init_poller();

//         let mut messages_demux = init_messages_demux(analyzer_channel_tx);

//         let reconnecting_client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );
//         messages_demux.add_client_channel(
//             reconnecting_client_key.clone(),
//             message_for_client_tx.clone(),
//             Span::current(),
//         );
//         // messages from 2chl6-4hpzw-vqaaa-aaaaa-c must be relayed as the client is registered in the poller

//         // simulating the case in which the poller did not yet receive the client channel for client ygoe7-xpj6n-24gsd-zksfw-2mywm-xfyop-yvlsp-ctlwa-753xv-wz6rk-uae
//         // messages from ygoe7-xpj6n-24gsd-zksfw-2mywm-xfyop-yvlsp-ctlwa-753xv-wz6rk-uae should be pushed to the queue

//         let mut messages = mock_messages_to_be_filtered();
//         let message_nonce = Arc::new(RwLock::new(0));
//         filter_canister_messages(
//             &mut messages,
//             *message_nonce.read().await,
//             reconnecting_client_key.clone(),
//         );
//         assert_eq!(messages.len(), 5);

//         let msgs = CanisterOutputCertifiedMessages {
//             messages: messages.clone(),
//             cert: Vec::new(),
//             tree: Vec::new(),
//         };

//         if let Err(e) = messages_demux
//             .relay_messages(msgs, message_nonce.clone(), Span::current().id())
//             .await
//         {
//             panic!("{:?}", e);
//         }

//         let mut received = 0;
//         let mut queued = 0;
//         for _ in 0..messages.len() {
//             match message_for_client_rx.try_recv() {
//                 Ok(update) => {
//                     if let IcWsConnectionUpdate::Message((m, _span)) = update {
//                         // counts the messages relayed should only be for client 2chl6-4hpzw-vqaaa-aaaaa-c
//                         // as it is the only one registered in the poller
//                         let websocket_message: WebsocketMessage = from_slice(&m.content)
//                             .expect("content must be of type WebsocketMessage");
//                         // only client 2chl6-4hpzw-vqaaa-aaaaa-c should be registered in poller
//                         assert_eq!(websocket_message.client_key, reconnecting_client_key);
//                         received += 1
//                     } else {
//                         panic!("updates must be of variant Message")
//                     }
//                 },
//                 Err(_) => queued += 1,
//             }
//         }
//         let expected_received = 3; // number of messages for 2chl6-4hpzw-vqaaa-aaaaa-c in mock_messages_to_be_filtered after gateway reboots
//         assert_eq!(received, expected_received);
//         let expected_queued = 2; // number of messages for ygoe7-xpj6n-24gsd-zksfw-2mywm-xfyop-yvlsp-ctlwa-753xv-wz6rk-uae
//         assert_eq!(queued, expected_queued);

//         let mut messages = mock_messages_to_be_filtered();
//         // here message_nonce is > 0, so messages will not be filtered
//         filter_canister_messages(
//             &mut messages,
//             *message_nonce.read().await,
//             reconnecting_client_key,
//         );
//         assert_eq!(messages.len(), 13);
//     }

//     #[tokio::test()]
//     /// Simulates the case in which the gateway polls a message for a client that is not yet registered in the poller.
//     /// Stores the message in the queue so that it can be processed later.
//     async fn should_push_message_to_queue() {
//         let (
//             _message_for_client_tx,
//             _message_for_client_rx,
//             analyzer_channel_tx,
//             _analyzer_channel_rx,
//         ) = init_poller();

//         let mut messages_demux = init_messages_demux(analyzer_channel_tx);

//         let client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );
//         let sequence_number = 0;
//         let canister_output_message = canister_open_message(&client_key, sequence_number);
//         let message_nonce = Arc::new(RwLock::new(0));

//         let msgs = CanisterOutputCertifiedMessages {
//             messages: vec![canister_output_message],
//             cert: Vec::new(),
//             tree: Vec::new(),
//         };

//         if let Err(e) = messages_demux
//             .relay_messages(msgs, message_nonce.clone(), Span::current().id())
//             .await
//         {
//             panic!("{:?}", e);
//         }

//         assert_eq!(messages_demux.count_clients_message_queues(), 1);
//     }

//     #[tokio::test()]
//     /// Simulates the case in which there is a message in the queue for a client that is connected.
//     /// Relays the message to the client and empties the queue.
//     async fn should_process_message_in_queue() {
//         let (
//             message_for_client_tx,
//             mut message_for_client_rx,
//             analyzer_channel_tx,
//             // the following have to be returned in order not to drop them
//             _analyzer_channel_rx,
//         ) = init_poller();

//         let mut messages_demux = init_messages_demux(analyzer_channel_tx);

//         let client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );
//         let sequence_number = 0;
//         let canister_output_message = canister_open_message(&client_key, sequence_number);
//         let client_key = canister_output_message.client_key;
//         let m = CanisterToClientMessage {
//             key: canister_output_message.key.clone(),
//             content: canister_output_message.content,
//             cert: Vec::new(),
//             tree: Vec::new(),
//         };
//         let incoming_canister_message_events = mock_incoming_canister_message_events();
//         messages_demux
//             .add_message_to_client_queue(&client_key, (m, incoming_canister_message_events));

//         // simulates the client being registered in the poller
//         messages_demux.add_client_channel(client_key, message_for_client_tx, Span::current());

//         messages_demux.process_queues(Span::current().id()).await;

//         if let None = message_for_client_rx.recv().await {
//             panic!("should receive message");
//         }

//         assert_eq!(messages_demux.count_clients_message_queues(), 0);
//     }

//     #[tokio::test()]
//     /// Simulates the case in which there is a message in the queue for a client that is not yet connected.
//     /// Keeps the message in the queue.
//     async fn should_keep_message_in_queue() {
//         let (
//             _message_for_client_tx,
//             _message_for_client_rx,
//             analyzer_channel_tx,
//             _analyzer_channel_rx,
//         ) = init_poller();

//         let mut messages_demux = init_messages_demux(analyzer_channel_tx);

//         let client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );
//         let sequence_number = 0;
//         let canister_output_message = canister_open_message(&client_key, sequence_number);
//         let client_key = canister_output_message.client_key;
//         let m = CanisterToClientMessage {
//             key: canister_output_message.key.clone(),
//             content: canister_output_message.content,
//             cert: Vec::new(),
//             tree: Vec::new(),
//         };
//         let incoming_canister_message_events = mock_incoming_canister_message_events();
//         messages_demux
//             .add_message_to_client_queue(&client_key, (m, incoming_canister_message_events));

//         messages_demux.process_queues(Span::current().id()).await;

//         assert_eq!(messages_demux.count_clients_message_queues(), 1);
//     }

//     #[tokio::test()]
//     /// Simulates the case in which there are multiple messages in the queue for a client that is connected.
//     /// Relays the messages to the client in ascending order specified by the sequence number.
//     async fn should_receive_messages_from_queue_in_order() {
//         let (
//             message_for_client_tx,
//             mut message_for_client_rx,
//             analyzer_channel_tx,
//             // the following have to be returned in order not to drop them
//             _analyzer_channel_rx,
//         ) = init_poller();

//         let mut messages_demux = init_messages_demux(analyzer_channel_tx);

//         let mut messages = Vec::new();
//         let client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );
//         let start_sequence_number = 0;
//         for canister_output_message in mock_ordered_messages(&client_key, start_sequence_number) {
//             let m = CanisterToClientMessage {
//                 key: canister_output_message.key.clone(),
//                 content: canister_output_message.content,
//                 cert: Vec::new(),
//                 tree: Vec::new(),
//             };
//             let incoming_canister_message_events = mock_incoming_canister_message_events();
//             messages.push((m, incoming_canister_message_events));
//         }

//         let count_messages = messages.len() as u64;
//         for message in messages {
//             messages_demux.add_message_to_client_queue(&client_key, message);
//         }

//         // simulates the client being registered in the poller
//         messages_demux.add_client_channel(
//             client_key.clone(),
//             message_for_client_tx,
//             Span::current(),
//         );

//         messages_demux.process_queues(Span::current().id()).await;

//         let mut expected_sequence_number = 0;
//         while let Ok(IcWsConnectionUpdate::Message((m, _span))) = message_for_client_rx.try_recv() {
//             let websocket_message: WebsocketMessage = from_slice(&m.content)
//                 .expect("content of canister_output_message is not of type WebsocketMessage");
//             assert_eq!(websocket_message.sequence_num, expected_sequence_number);
//             expected_sequence_number += 1;
//         }

//         // make sure that all messages are received
//         assert_eq!(count_messages, expected_sequence_number);
//         // make sure that no messages are pushed into the queue
//         assert_eq!(messages_demux.count_clients_message_queues(), 0);
//     }

//     #[tokio::test()]
//     /// Simulates the case in which the gateway polls multiple messages for a client that is connected.
//     /// Relays the messages to the client in ascending order specified by the sequence number.
//     async fn should_relay_polled_messages_in_order() {
//         let (
//             message_for_client_tx,
//             mut message_for_client_rx,
//             analyzer_channel_tx,
//             // the following have to be returned in order not to drop them
//             _analyzer_channel_rx,
//         ) = init_poller();

//         let mut messages_demux = init_messages_demux(analyzer_channel_tx);

//         let client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );
//         messages_demux.add_client_channel(
//             client_key.clone(),
//             message_for_client_tx,
//             Span::current(),
//         );

//         let start_sequence_number = 0;
//         let messages = mock_ordered_messages(&client_key, start_sequence_number);
//         let count_messages = messages.len() as u64;
//         let message_nonce = Arc::new(RwLock::new(0));
//         let msgs = CanisterOutputCertifiedMessages {
//             messages,
//             cert: Vec::new(),
//             tree: Vec::new(),
//         };

//         if let Err(e) = messages_demux
//             .relay_messages(msgs, message_nonce.clone(), Span::current().id())
//             .await
//         {
//             panic!("{:?}", e);
//         }

//         let mut expected_sequence_number = 0;
//         while let Ok(IcWsConnectionUpdate::Message((m, _span))) = message_for_client_rx.try_recv() {
//             let websocket_message: WebsocketMessage = from_slice(&m.content)
//                 .expect("content of canister_output_message is not of type WebsocketMessage");
//             assert_eq!(websocket_message.sequence_num, expected_sequence_number);
//             expected_sequence_number += 1;
//         }

//         // make sure that all messages are received
//         assert_eq!(count_messages, expected_sequence_number);
//         // make sure that no messages are pushed into the queue
//         assert_eq!(messages_demux.count_clients_message_queues(), 0);
//     }

//     #[tokio::test()]
//     /// Simulates the case in which the gateway polls multiple messages for a client that is connected while there are already multiple messages in the queue.
//     /// Relays the messages to the client in ascending order specified by the sequence number.
//     async fn should_relay_polled_messages_in_order_after_processing_queue() {
//         let (
//             message_for_client_tx,
//             mut message_for_client_rx,
//             analyzer_channel_tx,
//             // the following have to be returned in order not to drop them
//             _analyzer_channel_rx,
//         ) = init_poller();

//         let mut messages_demux = init_messages_demux(analyzer_channel_tx);

//         let client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );
//         messages_demux.add_client_channel(client_key, message_for_client_tx, Span::current());

//         let mut messages_in_queue = Vec::new();
//         let client_key = ClientKey::new(
//             Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap(),
//             0,
//         );
//         let start_sequence_number = 0;
//         for canister_output_message in mock_ordered_messages(&client_key, start_sequence_number) {
//             let m = CanisterToClientMessage {
//                 key: canister_output_message.key.clone(),
//                 content: canister_output_message.content,
//                 cert: Vec::new(),
//                 tree: Vec::new(),
//             };
//             let incoming_canister_message_events = mock_incoming_canister_message_events();
//             messages_in_queue.push((m, incoming_canister_message_events));
//         }

//         let count_messages_in_queue = messages_in_queue.len() as u64;
//         for message in messages_in_queue {
//             messages_demux.add_message_to_client_queue(&client_key, message);
//         }

//         messages_demux.process_queues(Span::current().id()).await;

//         let start_sequence_number = count_messages_in_queue;
//         let polled_messages = mock_ordered_messages(&client_key, start_sequence_number);
//         let count_polled_messages = polled_messages.len() as u64;
//         let message_nonce = Arc::new(RwLock::new(0));

//         let msgs = CanisterOutputCertifiedMessages {
//             messages: polled_messages,
//             cert: Vec::new(),
//             tree: Vec::new(),
//         };

//         if let Err(e) = messages_demux
//             .relay_messages(msgs, message_nonce.clone(), Span::current().id())
//             .await
//         {
//             panic!("{:?}", e);
//         }

//         let mut expected_sequence_number = 0;
//         while let Ok(IcWsConnectionUpdate::Message((m, _span))) = message_for_client_rx.try_recv() {
//             let websocket_message: WebsocketMessage = from_slice(&m.content)
//                 .expect("content of canister_output_message is not of type WebsocketMessage");
//             assert_eq!(websocket_message.sequence_num, expected_sequence_number);
//             expected_sequence_number += 1;
//         }

//         // make sure that all messages are received
//         assert_eq!(
//             count_messages_in_queue + count_polled_messages,
//             expected_sequence_number
//         );
//         // make sure that no messages are pushed into the queue
//         assert_eq!(messages_demux.count_clients_message_queues(), 0);
//     }
// }

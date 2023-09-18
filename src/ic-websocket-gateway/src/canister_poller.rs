use crate::{
    canister_methods::{
        self, CanisterOpenMessageContent, CanisterOutputCertifiedMessages, CanisterOutputMessage,
        CanisterServiceMessage, CanisterWsGetMessagesArguments, ClientPrincipal, WebsocketMessage,
    },
    events_analyzer::{Events, EventsCollectionType, EventsReference},
    metrics::canister_poller_metrics::{
        IncomingCanisterMessageEvents, IncomingCanisterMessageEventsMetrics, PollerEvents,
        PollerEventsMetrics,
    },
};
use candid::{decode_one, CandidType};
use ic_agent::{export::Principal, Agent};
use serde::{Deserialize, Serialize};
use serde_cbor::from_slice;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    join, select,
    sync::mpsc::{Receiver, Sender},
};
use tracing::{debug, error, info, warn};

type CanisterGetMessagesWithEvents = (CanisterOutputCertifiedMessages, PollerEvents);

#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct CanisterToClientMessage {
    pub key: String,
    #[serde(with = "serde_bytes")]
    pub content: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub cert: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub tree: Vec<u8>,
}

/// ends of the channels needed by each canister poller tasks:
/// - main_to_poller: receiving side of the channel used by the main task to send the receiving task of a new client's channel to the poller task
/// - poller_to_main: sending side of the channel used by the poller to send the canister id of the poller which is about to terminate
#[derive(Debug)]
pub struct PollerChannelsPollerEnds {
    main_to_poller: Receiver<PollerToClientChannelData>,
    poller_to_main: Sender<TerminationInfo>,
    poller_to_analyzer: Sender<Box<dyn Events + Send>>,
}

impl PollerChannelsPollerEnds {
    pub fn new(
        main_to_poller: Receiver<PollerToClientChannelData>,
        poller_to_main: Sender<TerminationInfo>,
        poller_to_analyzer: Sender<Box<dyn Events + Send>>,
    ) -> Self {
        Self {
            main_to_poller,
            poller_to_main,
            poller_to_analyzer,
        }
    }
}

pub enum IcWsConnectionUpdate {
    Message(CanisterToClientMessage),
    Error(String),
}

/// contains the information that the main sends to the poller task:
/// - NewClientChannel: sending side of the channel use by the poller to send messages to the client
/// - ClientDisconnected: signals the poller which cllient disconnected
#[derive(Debug, Clone)]
pub enum PollerToClientChannelData {
    NewClientChannel(ClientPrincipal, Sender<IcWsConnectionUpdate>),
    ClientDisconnected(ClientPrincipal),
}

/// determines the reason of the poller task termination:
/// - LastClientDisconnected: last client disconnected and therefore there is no need to continue polling
/// - CdkError: error while polling the canister
pub enum TerminationInfo {
    LastClientDisconnected(Principal),
    CdkError(Principal),
}

pub struct CanisterPoller {
    canister_id: Principal,
    agent: Arc<Agent>,
    polling_interval_ms: u64,
}

impl CanisterPoller {
    pub fn new(canister_id: Principal, agent: Arc<Agent>, polling_interval_ms: u64) -> Self {
        Self {
            canister_id,
            agent,
            polling_interval_ms,
        }
    }

    #[tracing::instrument(
        name = "poll_canister",
        skip_all,
        fields(
            canister_id = %self.canister_id
        )
    )]
    pub async fn run_polling(&self, mut poller_channels: PollerChannelsPollerEnds) {
        // once the poller starts running, it requests messages from nonce 0.
        // if the canister already has some messages in the queue and receives the nonce 0, it knows that the poller restarted
        // therefore, it sends the last X messages to the gateway. From these, the gateway has to determine the response corresponding to the client's ws_open request
        let mut message_nonce = 0;

        // channels used to communicate with client's WebSocket task
        let mut client_channels: HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>> =
            HashMap::new();

        let mut clients_message_queues: HashMap<ClientPrincipal, Vec<CanisterToClientMessage>> =
            HashMap::new();

        let mut polling_iteration = 0; // used as a reference for the PollerEvents
        let get_messages_operation = self.get_canister_updates(message_nonce, polling_iteration);
        // pin the tracking of the in-flight asynchronous operation so that in each select! iteration get_messages_operation is continued
        // instead of issuing a new call to get_canister_updates
        tokio::pin!(get_messages_operation);

        'poller_loop: loop {
            select! {
                // receive channel used to send canister updates to new client's task
                Some(channel_data) = poller_channels.main_to_poller.recv() => {
                    match channel_data {
                        PollerToClientChannelData::NewClientChannel(client_principal, client_channel) => {
                            debug!("Added new channel to poller for client: {:?}", client_principal);
                            client_channels.insert(client_principal.clone(), client_channel);
                        },
                        PollerToClientChannelData::ClientDisconnected(client_principal) => {
                            debug!("Removed client channel from poller for client {:?}", client_principal);
                            client_channels.remove(&client_principal);
                            debug!("Removed message queue from poller for client {:?}", client_principal);
                            clients_message_queues.remove(&client_principal);
                            debug!("{} clients connected to poller", client_channels.len());
                            // exit task if last client disconnected
                            if client_channels.is_empty() {
                                info!("Terminating poller task as no clients are connected");
                                signal_poller_task_termination(&mut poller_channels.poller_to_main, TerminationInfo::LastClientDisconnected(self.canister_id)).await;
                                break;
                            }
                        }
                    }
                }
                // poll canister for updates across multiple select! iterations
                res = &mut get_messages_operation => {
                    // process messages in queues before the ones just polled from the canister (if any) so that the clients receive messages in the expected order
                    // this is done even if no messages are returned from the current polling iteration as there might be messages in the queue waiting to be processed
                    process_queues(&mut clients_message_queues, &client_channels).await;

                    if let Some((mut msgs, mut poller_events)) = res {
                        poller_events.metrics.set_start_relaying_messages();
                        poller_channels.poller_to_analyzer.send(Box::new(poller_events)).await.expect("analyzer's side of the channel dropped");

                        process_canister_messages(
                            &mut msgs.messages,
                            message_nonce
                        );
                        for canister_output_message in msgs.messages {
                            if let Err(e) = relay_message(
                                canister_output_message,
                                msgs.cert.clone(),
                                msgs.tree.clone(),
                                &client_channels,
                                &poller_channels,
                                &mut clients_message_queues,
                                &mut message_nonce,
                            )
                            .await {
                                error!("Terminating poller task due to CDK error: {}", e);
                                signal_termination_and_cleanup(
                                    &mut poller_channels.poller_to_main,
                                    self.canister_id,
                                    &client_channels,
                                    e,
                                )
                                .await;
                                break 'poller_loop;
                            }
                        }
                        // counting only iterations which return at least one canister message
                        polling_iteration += 1;
                    }

                    // pin a new asynchronous operation so that it can be restarted in the next select! iteration and continued in the following ones
                    get_messages_operation.set(self.get_canister_updates(message_nonce, polling_iteration));
                },
            }
        }
    }

    async fn get_canister_updates(
        &self,
        message_nonce: u64,
        polling_iteration: u64,
    ) -> Option<CanisterGetMessagesWithEvents> {
        let mut poller_events = PollerEvents::new(
            Some(EventsReference::Iteration(polling_iteration)),
            EventsCollectionType::PollerStatus,
            PollerEventsMetrics::default(),
        );
        poller_events.metrics.set_start_polling();
        sleep(self.polling_interval_ms).await;
        // get messages to be relayed to clients from canister (starting from 'message_nonce')
        let canister_result = canister_methods::ws_get_messages(
            &self.agent,
            &self.canister_id,
            CanisterWsGetMessagesArguments {
                nonce: message_nonce,
            },
        )
        .await
        .ok()?;
        poller_events.metrics.set_received_messages();
        if canister_result.messages.len() > 0 {
            return Some((canister_result, poller_events));
        }
        None
    }
}

fn process_canister_messages<'a>(messages: &'a mut Vec<CanisterOutputMessage>, message_nonce: u64) {
    if message_nonce != 0 {
        // this is not the first polling iteration and therefore the poller queried the canister starting from the nonce of the last message of the previous polling iteration
        // therefore, all the received messages are new and have to be relayed to the respective client handlers
        return;
    } else {
        // if the poller just started (message_nonce == 0), the canister might have already had other messages in the queue which we should not send to the clients
        // therefore, starting from the last message polled, we relay the open message of type CanisterServiceMessage::OpenMessage for each connected client
        // message_nonce has to be set to the nonce of the last open message pollled in this iteration so that in the next iteration we can poll from there
        filter_messages_of_first_polling_iteration(messages)
    }
}

async fn relay_message(
    canister_output_message: CanisterOutputMessage,
    cert: Vec<u8>,
    tree: Vec<u8>,
    client_channels: &HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>>,
    poller_channels: &PollerChannelsPollerEnds,
    clients_message_queues: &mut HashMap<ClientPrincipal, Vec<CanisterToClientMessage>>,
    message_nonce: &mut u64,
) -> Result<(), String> {
    let mut incoming_canister_message_events = IncomingCanisterMessageEvents::new(
        None,
        EventsCollectionType::CanisterMessage,
        IncomingCanisterMessageEventsMetrics::default(),
    );
    incoming_canister_message_events
        .metrics
        .set_start_relaying_message();
    let client_principal = canister_output_message.client_principal;
    let m = CanisterToClientMessage {
        key: canister_output_message.key.clone(),
        content: canister_output_message.content,
        cert,
        tree,
    };

    let last_message_nonce = get_nonce_from_message(&canister_output_message.key)?;
    incoming_canister_message_events.reference =
        Some(EventsReference::MessageNonce(last_message_nonce));
    match client_channels.get(&client_principal) {
        Some(client_channel_tx) => {
            debug!("Received message with key: {:?} from canister", m.key);
            if let Err(e) = client_channel_tx
                .send(IcWsConnectionUpdate::Message(m))
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
            poller_channels
                .poller_to_analyzer
                .send(Box::new(incoming_canister_message_events))
                .await
                .expect("analyzer's side of the channel dropped");
        },
        None => {
            // TODO: we should distinguish the case in which there is no client channel because the client's state hasn't been registered (yet)
            //       from the case in which the client has just disconnected (and its client channel removed before the polling returns new messages fot that client)
            warn!("Connection to client with principal: {:?} not opened yet. Adding message with key: {:?} to queue", m.key, client_principal);
            if let Some(message_queue) = clients_message_queues.get_mut(&client_principal) {
                message_queue.push(m);
            } else {
                clients_message_queues.insert(client_principal, vec![m]);
            }
        },
    }
    *message_nonce = last_message_nonce + 1;
    Ok(())
}

fn filter_messages_of_first_polling_iteration<'a>(messages: &'a mut Vec<CanisterOutputMessage>) {
    // eliminating the undesired messages in-place has the advantage that we do not have to clone every message when relaying it to the client handler
    // however, this requires decoding every message polled in the first iteration
    // as this happens only once (per reboot), it's a good trad off

    // TODO: if for some of the connected clients there are also messages other than the open message, these must also be relayed
    //       !!! the current implementation skips these messages if they are inserted in the queue before the last open message polled in the first iteration !!!

    // TODO: the polled messages might contain "old" open messages which are not filtered out
    //       these should not be pushed into the queue as the client that sent them is already disconnected
    messages.retain(|canister_output_message| {
        let websocket_message: WebsocketMessage = from_slice(&canister_output_message.content)
            .expect("content of canister_output_message is not of type WebsocketMessage");
        if websocket_message.is_service_message {
            let canister_service_message = decode_one(&websocket_message.content)
                .expect("content of websocket_message is not of type CanisterServiceMessage");
            if let CanisterServiceMessage::OpenMessage(CanisterOpenMessageContent { .. }) =
                canister_service_message
            {
                return true;
            }
        }
        false
    })
}

async fn process_queues(
    clients_message_queues: &mut HashMap<ClientPrincipal, Vec<CanisterToClientMessage>>,
    client_channels: &HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>>,
) {
    // TODO: make sure that messages are delivered to each client in the order defined by their sequence numbers
    //       might not be the case as messages are sent concurrently

    let mut handles = Vec::new();
    clients_message_queues.retain(|client_principal, message_queue| {
        if let Some(client_channel_tx) = client_channels.get(&client_principal) {
            // once a client channel is received, messages for that client will not be put in the queue anymore (until that client disconnects)
            // thus the respective queue does not need to be stored
            for m in message_queue.to_owned() {
                let client_channel_tx = client_channel_tx.clone();
                let handle = tokio::spawn(async move {
                    warn!("Processing message with key: {:?} from queue", m.key);
                    if let Err(e) = client_channel_tx
                        .send(IcWsConnectionUpdate::Message(m))
                        .await
                    {
                        error!("Client's thread terminated: {}", e);
                    }
                });
                handles.push(handle);
            }
            return false;
        }
        // if the client channel has not been received yet, keep the messages in the queue
        true
    });
    // the tasks must be awaited so that messages in queue are relayed before newly polled messages
    for handle in handles {
        let (_,) = join!(handle);
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

async fn signal_termination_and_cleanup(
    poller_to_main_channel: &mut Sender<TerminationInfo>,
    canister_id: Principal,
    client_channels: &HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>>,
    e: String,
) {
    // let the main task know that this poller will terminate due to a CDK error
    signal_poller_task_termination(
        poller_to_main_channel,
        TerminationInfo::CdkError(canister_id),
    )
    .await;
    // let each client connection handler task connected to this poller know that the poller will terminate
    // and thus they also have to close the WebSocket connection and terminate
    for client_channel_tx in client_channels.values() {
        if let Err(channel_err) = client_channel_tx
            .send(IcWsConnectionUpdate::Error(format!(
                "Terminating poller task due to error: {}",
                e
            )))
            .await
        {
            error!("Client's thread terminated: {}", channel_err);
        }
    }
}

async fn signal_poller_task_termination(
    channel: &mut Sender<TerminationInfo>,
    info: TerminationInfo,
) {
    if let Err(e) = channel.send(info).await {
        error!(
            "Receiver has been dropped on the pollers connection manager's side. Error: {:?}",
            e
        );
    }
}

async fn sleep(millis: u64) {
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::canister_methods::{
        CanisterAckMessageContent, CanisterOpenMessageContent, CanisterOutputMessage,
        CanisterServiceMessage, ClientPrincipal, WebsocketMessage,
    };
    use crate::canister_poller::{
        process_canister_messages, relay_message, CanisterToClientMessage, IcWsConnectionUpdate,
        PollerChannelsPollerEnds, TerminationInfo,
    };
    use crate::events_analyzer::Events;
    use candid::{encode_one, Principal};
    use serde::Serialize;
    use serde_cbor::{from_slice, Serializer};
    use tokio::sync::mpsc::{self, Receiver, Sender};

    use super::{process_queues, PollerToClientChannelData};

    fn init_poller() -> (
        Sender<IcWsConnectionUpdate>,
        Receiver<IcWsConnectionUpdate>,
        HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>>,
        PollerChannelsPollerEnds,
        HashMap<ClientPrincipal, Vec<CanisterToClientMessage>>,
        Receiver<Box<dyn Events + Send>>,
        Sender<PollerToClientChannelData>,
        Receiver<TerminationInfo>,
    ) {
        let (message_for_client_tx, message_for_client_rx): (
            Sender<IcWsConnectionUpdate>,
            Receiver<IcWsConnectionUpdate>,
        ) = mpsc::channel(100);

        let client_channels: HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>> =
            HashMap::new();

        let (events_channel_tx, events_channel_rx) = mpsc::channel(100);

        let (
            poller_channel_for_client_channel_sender_tx,
            poller_channel_for_client_channel_sender_rx,
        ) = mpsc::channel(100);

        let (poller_channel_for_completion_tx, poller_channel_for_completion_rx): (
            Sender<TerminationInfo>,
            Receiver<TerminationInfo>,
        ) = mpsc::channel(100);

        let poller_channels_poller_ends = PollerChannelsPollerEnds::new(
            poller_channel_for_client_channel_sender_rx,
            poller_channel_for_completion_tx,
            events_channel_tx.clone(),
        );

        let clients_message_queues: HashMap<ClientPrincipal, Vec<CanisterToClientMessage>> =
            HashMap::new();

        (
            message_for_client_tx,
            message_for_client_rx,
            client_channels,
            poller_channels_poller_ends,
            clients_message_queues,
            events_channel_rx,
            poller_channel_for_client_channel_sender_tx,
            poller_channel_for_completion_rx,
        )
    }

    fn cbor_serialize<T: Serialize>(m: T) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut serializer = Serializer::new(&mut bytes);
        serializer.self_describe().unwrap();
        m.serialize(&mut serializer).unwrap();
        bytes
    }

    fn mock_open_message(client_principal: Principal) -> CanisterServiceMessage {
        CanisterServiceMessage::OpenMessage(CanisterOpenMessageContent { client_principal })
    }

    fn mock_ack_message() -> CanisterServiceMessage {
        CanisterServiceMessage::AckMessage(CanisterAckMessageContent {
            last_incoming_sequence_num: 0,
        })
    }

    fn mock_websocket_service_message(
        content: CanisterServiceMessage,
        client_principal: Principal,
        sequence_num: u64,
    ) -> WebsocketMessage {
        WebsocketMessage {
            client_principal,
            sequence_num,
            timestamp: 0,
            is_service_message: true,
            content: encode_one(content).unwrap(),
        }
    }

    fn mock_websocket_message(client_principal: Principal, sequence_num: u64) -> WebsocketMessage {
        WebsocketMessage {
            client_principal,
            sequence_num,
            timestamp: 0,
            is_service_message: false,
            content: vec![],
        }
    }

    fn mock_canister_output_message(
        content: WebsocketMessage,
        client_principal: Principal,
    ) -> CanisterOutputMessage {
        CanisterOutputMessage {
            client_principal,
            key: String::from("gateway_uid_0"),
            content: cbor_serialize(content),
        }
    }

    fn canister_open_message(
        client_principal: Principal,
        sequence_num: u64,
    ) -> CanisterOutputMessage {
        let open_message = mock_open_message(client_principal);
        let websocket_service_message =
            mock_websocket_service_message(open_message, client_principal, sequence_num);
        mock_canister_output_message(websocket_service_message, client_principal)
    }

    fn canister_ack_message(
        client_principal: Principal,
        sequence_num: u64,
    ) -> CanisterOutputMessage {
        let ack_message = mock_ack_message();
        let websocket_service_message =
            mock_websocket_service_message(ack_message, client_principal, sequence_num);
        mock_canister_output_message(websocket_service_message, client_principal)
    }

    fn canister_output_message(
        client_principal: Principal,
        sequence_num: u64,
    ) -> CanisterOutputMessage {
        let websocket_message = mock_websocket_message(client_principal, sequence_num);
        mock_canister_output_message(websocket_message, client_principal)
    }

    fn mock_messages_to_be_filtered() -> Vec<CanisterOutputMessage> {
        let client_principal = Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap();
        let mut sequence_number = 0;

        let mut messages = Vec::new();

        // this message should be filtered out
        let canister_message = canister_output_message(client_principal, sequence_number);
        messages.push(canister_message);
        sequence_number += 1;

        // this message should be filtered out
        let canister_message = canister_ack_message(client_principal, sequence_number);
        messages.push(canister_message);
        sequence_number += 1;

        // this message should be filtered out
        let canister_message = canister_output_message(client_principal, sequence_number);
        messages.push(canister_message);
        sequence_number += 1;

        // this message should not be filtered out
        let canister_message = canister_open_message(client_principal, sequence_number);
        messages.push(canister_message);
        sequence_number += 1;

        // this message should be filtered out
        let canister_message = canister_ack_message(client_principal, sequence_number);
        messages.push(canister_message);
        sequence_number += 1;

        // this message should not be filtered out
        let canister_message = canister_open_message(client_principal, sequence_number);
        messages.push(canister_message);
        sequence_number += 1;

        // this message should be filtered out
        let canister_message = canister_output_message(client_principal, sequence_number);
        messages.push(canister_message);
        sequence_number += 1;

        // this message should be filtered out
        let canister_message = canister_ack_message(client_principal, sequence_number);
        messages.push(canister_message);
        sequence_number += 1;

        // this message should be filtered out
        let canister_message = canister_output_message(client_principal, sequence_number);
        messages.push(canister_message);
        sequence_number += 1;

        // this message should not be filtered out
        let canister_message = canister_open_message(client_principal, sequence_number);
        messages.push(canister_message);

        messages
    }

    fn mock_ordered_messages(
        client_principal: Principal,
        start_sequence_number: u64,
    ) -> Vec<CanisterOutputMessage> {
        let mut sequence_number = start_sequence_number;

        let mut messages = Vec::new();
        while sequence_number < 10 {
            let canister_message = canister_output_message(client_principal, sequence_number);
            messages.push(canister_message);
            sequence_number += 1;
        }
        messages
    }

    #[tokio::test()]
    /// Simulates the case in which the poller starts and the canister's queue contains some old messages.
    /// Relays only open messages for the connected clients.
    async fn should_process_canister_messages() {
        let (
            message_for_client_tx,
            mut message_for_client_rx,
            mut client_channels,
            poller_channels_poller_ends,
            mut clients_message_queues,
            // the following have to be returned in order not to drop them
            _events_channel_rx,
            _poller_channel_for_client_channel_sender_tx,
            _poller_channel_for_completion_rx,
        ) = init_poller();

        let client_principal = Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap();
        client_channels.insert(client_principal, message_for_client_tx);

        let mut messages = mock_messages_to_be_filtered();
        let mut message_nonce = 0;
        process_canister_messages(&mut messages, message_nonce);
        assert_eq!(messages.len(), 3);

        for canister_output_message in messages {
            relay_message(
                canister_output_message,
                Vec::new(),
                Vec::new(),
                &client_channels,
                &poller_channels_poller_ends,
                &mut clients_message_queues,
                &mut message_nonce,
            )
            .await
            .unwrap();
            if let Err(_) = message_for_client_rx.try_recv() {
                panic!("should not receive error");
            }
        }

        let mut messages = mock_messages_to_be_filtered();
        // here message_nonce is > 0, so messages will not be filtered
        process_canister_messages(&mut messages, message_nonce);
        assert_eq!(messages.len(), 10);
    }

    #[tokio::test()]
    /// Simulates the case in which the gateway polls a message for a client that is not yet registered in the poller.
    /// Stores the message in the queue so that it can be processed later.
    async fn should_push_message_to_queue() {
        let (
            _message_for_client_tx,
            _message_for_client_rx,
            client_channels,
            poller_channels_poller_ends,
            mut clients_message_queues,
            // the following have to be returned in order not to drop them
            _events_channel_rx,
            _poller_channel_for_client_channel_sender_tx,
            _poller_channel_for_completion_rx,
        ) = init_poller();

        let client_principal = Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap();
        let sequence_number = 0;
        let canister_output_message = canister_open_message(client_principal, sequence_number);
        let mut message_nonce = 0;

        relay_message(
            canister_output_message,
            Vec::new(),
            Vec::new(),
            &client_channels,
            &poller_channels_poller_ends,
            &mut clients_message_queues,
            &mut message_nonce,
        )
        .await
        .unwrap();

        assert_eq!(clients_message_queues.len(), 1);
    }

    #[tokio::test()]
    /// Simulates the case in which there is a message in the queue for a client that is connected.
    /// Relays the message to the client and empties the queue.
    async fn should_process_message_in_queue() {
        let (
            message_for_client_tx,
            mut message_for_client_rx,
            mut client_channels,
            _poller_channels_poller_ends,
            mut clients_message_queues,
            // the following have to be returned in order not to drop them
            _events_channel_rx,
            _poller_channel_for_client_channel_sender_tx,
            _poller_channel_for_completion_rx,
        ) = init_poller();

        let client_principal = Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap();
        let sequence_number = 0;
        let canister_output_message = canister_open_message(client_principal, sequence_number);
        let client_principal = canister_output_message.client_principal;
        let m = CanisterToClientMessage {
            key: canister_output_message.key.clone(),
            content: canister_output_message.content,
            cert: Vec::new(),
            tree: Vec::new(),
        };
        clients_message_queues.insert(client_principal, vec![m]);

        // simulates the client being registered in the poller
        client_channels.insert(client_principal, message_for_client_tx);

        process_queues(&mut clients_message_queues, &client_channels).await;

        if let None = message_for_client_rx.recv().await {
            panic!("should receive message");
        }

        assert_eq!(clients_message_queues.len(), 0);
    }

    #[tokio::test()]
    /// Simulates the case in which there is a message in the queue for a client that is not yet connected.
    /// Keeps the message in the queue.
    async fn should_keep_message_in_queue() {
        let (
            _message_for_client_tx,
            _message_for_client_rx,
            client_channels,
            _poller_channels_poller_ends,
            mut clients_message_queues,
            // the following have to be returned in order not to drop them
            _events_channel_rx,
            _poller_channel_for_client_channel_sender_tx,
            _poller_channel_for_completion_rx,
        ) = init_poller();

        let client_principal = Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap();
        let sequence_number = 0;
        let canister_output_message = canister_open_message(client_principal, sequence_number);
        let client_principal = canister_output_message.client_principal;
        let m = CanisterToClientMessage {
            key: canister_output_message.key.clone(),
            content: canister_output_message.content,
            cert: Vec::new(),
            tree: Vec::new(),
        };
        clients_message_queues.insert(client_principal, vec![m]);

        process_queues(&mut clients_message_queues, &client_channels).await;

        assert_eq!(clients_message_queues.len(), 1);
    }

    #[tokio::test()]
    /// Simulates the case in which there are multiple messages in the queue for a client that is connected.
    /// Relays the messages to the client in ascending order specified by the sequence number.
    async fn should_receive_messages_from_queue_in_order() {
        let (
            message_for_client_tx,
            mut message_for_client_rx,
            mut client_channels,
            _poller_channels_poller_ends,
            mut clients_message_queues,
            // the following have to be returned in order not to drop them
            _events_channel_rx,
            _poller_channel_for_client_channel_sender_tx,
            _poller_channel_for_completion_rx,
        ) = init_poller();

        let mut messages = Vec::new();
        let client_principal = Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap();
        let start_sequence_number = 0;
        for canister_output_message in
            mock_ordered_messages(client_principal, start_sequence_number)
        {
            let m = CanisterToClientMessage {
                key: canister_output_message.key.clone(),
                content: canister_output_message.content,
                cert: Vec::new(),
                tree: Vec::new(),
            };
            messages.push(m);
        }

        let count_messages = messages.len() as u64;
        clients_message_queues.insert(client_principal, messages);

        // simulates the client being registered in the poller
        client_channels.insert(client_principal, message_for_client_tx);

        process_queues(&mut clients_message_queues, &client_channels).await;

        let mut expected_sequence_number = 0;
        while let Ok(IcWsConnectionUpdate::Message(m)) = message_for_client_rx.try_recv() {
            let websocket_message: WebsocketMessage = from_slice(&m.content)
                .expect("content of canister_output_message is not of type WebsocketMessage");
            assert_eq!(websocket_message.sequence_num, expected_sequence_number);
            expected_sequence_number += 1;
        }

        // make sure that all messages are received
        assert_eq!(count_messages, expected_sequence_number);
        // make sure that no messages are pushed into the queue
        assert_eq!(clients_message_queues.len(), 0);
    }

    #[tokio::test()]
    /// Simulates the case in which the gateway polls multiple messages for a client that is connected.
    /// Relays the messages to the client in ascending order specified by the sequence number.
    async fn should_relay_polled_messages_in_order() {
        let (
            message_for_client_tx,
            mut message_for_client_rx,
            mut client_channels,
            poller_channels_poller_ends,
            mut clients_message_queues,
            // the following have to be returned in order not to drop them
            _events_channel_rx,
            _poller_channel_for_client_channel_sender_tx,
            _poller_channel_for_completion_rx,
        ) = init_poller();

        let client_principal = Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap();
        client_channels.insert(client_principal, message_for_client_tx);

        let start_sequence_number = 0;
        let messages = mock_ordered_messages(client_principal, start_sequence_number);
        let count_messages = messages.len() as u64;
        let mut message_nonce = 0;
        for canister_output_message in messages {
            relay_message(
                canister_output_message,
                Vec::new(),
                Vec::new(),
                &client_channels,
                &poller_channels_poller_ends,
                &mut clients_message_queues,
                &mut message_nonce,
            )
            .await
            .unwrap();
        }

        let mut expected_sequence_number = 0;
        while let Ok(IcWsConnectionUpdate::Message(m)) = message_for_client_rx.try_recv() {
            let websocket_message: WebsocketMessage = from_slice(&m.content)
                .expect("content of canister_output_message is not of type WebsocketMessage");
            assert_eq!(websocket_message.sequence_num, expected_sequence_number);
            expected_sequence_number += 1;
        }

        // make sure that all messages are received
        assert_eq!(count_messages, expected_sequence_number);
        // make sure that no messages are pushed into the queue
        assert_eq!(clients_message_queues.len(), 0);
    }

    #[tokio::test()]
    /// Simulates the case in which the gateway polls multiple messages for a client that is connected while there are already multiple messages in the queue.
    /// Relays the messages to the client in ascending order specified by the sequence number.
    async fn should_relay_polled_messages_in_order_after_processing_queue() {
        let (
            message_for_client_tx,
            mut message_for_client_rx,
            mut client_channels,
            poller_channels_poller_ends,
            mut clients_message_queues,
            // the following have to be returned in order not to drop them
            _events_channel_rx,
            _poller_channel_for_client_channel_sender_tx,
            _poller_channel_for_completion_rx,
        ) = init_poller();

        let client_principal = Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap();
        client_channels.insert(client_principal, message_for_client_tx);

        let mut messages_in_queue = Vec::new();
        let client_principal = Principal::from_text("2chl6-4hpzw-vqaaa-aaaaa-c").unwrap();
        let start_sequence_number = 0;
        for canister_output_message in
            mock_ordered_messages(client_principal, start_sequence_number)
        {
            let m = CanisterToClientMessage {
                key: canister_output_message.key.clone(),
                content: canister_output_message.content,
                cert: Vec::new(),
                tree: Vec::new(),
            };
            messages_in_queue.push(m);
        }

        let count_messages_in_queue = messages_in_queue.len() as u64;
        clients_message_queues.insert(client_principal, messages_in_queue);

        process_queues(&mut clients_message_queues, &client_channels).await;

        let start_sequence_number = count_messages_in_queue;
        let polled_messages = mock_ordered_messages(client_principal, start_sequence_number);
        let count_polled_messages = polled_messages.len() as u64;
        let mut message_nonce = 0;
        for canister_output_message in polled_messages {
            relay_message(
                canister_output_message,
                Vec::new(),
                Vec::new(),
                &client_channels,
                &poller_channels_poller_ends,
                &mut clients_message_queues,
                &mut message_nonce,
            )
            .await
            .unwrap();
        }

        let mut expected_sequence_number = 0;
        while let Ok(IcWsConnectionUpdate::Message(m)) = message_for_client_rx.try_recv() {
            let websocket_message: WebsocketMessage = from_slice(&m.content)
                .expect("content of canister_output_message is not of type WebsocketMessage");
            assert_eq!(websocket_message.sequence_num, expected_sequence_number);
            expected_sequence_number += 1;
        }

        // make sure that all messages are received
        assert_eq!(
            count_messages_in_queue + count_polled_messages,
            expected_sequence_number
        );
        // make sure that no messages are pushed into the queue
        assert_eq!(clients_message_queues.len(), 0);
    }
}

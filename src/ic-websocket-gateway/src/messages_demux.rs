use std::collections::HashMap;

use tokio::sync::mpsc::Sender;
use tracing::{error, trace, warn};

use crate::{
    canister_methods::{CanisterOutputCertifiedMessages, CanisterToClientMessage, ClientKey},
    canister_poller::{IcWsConnectionUpdate, PollerChannelsPollerEnds},
    events_analyzer::{EventsCollectionType, EventsReference},
    metrics::canister_poller_metrics::{
        IncomingCanisterMessageEvents, IncomingCanisterMessageEventsMetrics,
    },
};

pub struct MessagesDemux {}

impl MessagesDemux {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn relay_messages(
        &self,
        msgs: CanisterOutputCertifiedMessages,
        clients_message_queues: &mut HashMap<ClientKey, Vec<CanisterToClientMessage>>,
        client_channels: &HashMap<ClientKey, Sender<IcWsConnectionUpdate>>,
        poller_channels: &mut PollerChannelsPollerEnds,
        message_nonce: &mut u64,
    ) -> Result<(), String> {
        for canister_output_message in msgs.messages {
            let canister_to_client_message = CanisterToClientMessage {
                key: canister_output_message.key,
                content: canister_output_message.content,
                cert: msgs.cert.clone(),
                tree: msgs.tree.clone(),
            };
            if let Err(e) = relay_message(
                canister_output_message.client_key,
                canister_to_client_message,
                &client_channels,
                &poller_channels,
                clients_message_queues,
                message_nonce,
            )
            .await
            {
                return Err(format!("Terminating poller task due to CDK error: {}", e));
            }
        }
        Ok(())
    }
}

pub async fn relay_message(
    client_key: ClientKey,
    canister_to_client_message: CanisterToClientMessage,
    client_channels: &HashMap<ClientKey, Sender<IcWsConnectionUpdate>>,
    poller_channels: &PollerChannelsPollerEnds,
    clients_message_queues: &mut HashMap<ClientKey, Vec<CanisterToClientMessage>>,
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

    let last_message_nonce = get_nonce_from_message(&canister_to_client_message.key)?;
    incoming_canister_message_events.reference =
        Some(EventsReference::MessageNonce(last_message_nonce));
    match client_channels.get(&client_key) {
        Some(client_channel_tx) => {
            trace!(
                "Received message with key: {:?} from canister",
                canister_to_client_message.key
            );
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
            poller_channels
                .poller_to_analyzer
                .send(Box::new(incoming_canister_message_events))
                .await
                .expect("analyzer's side of the channel dropped");
        },
        None => {
            // TODO: we should distinguish the case in which there is no client channel because the client's state hasn't been registered (yet)
            //       from the case in which the client has just disconnected (and its client channel removed before the polling returns new messages fot that client)
            warn!("Connection to client with principal: {:?} not opened yet. Adding message with key: {:?} to queue", canister_to_client_message.key, client_key);
            if let Some(message_queue) = clients_message_queues.get_mut(&client_key) {
                message_queue.push(canister_to_client_message);
            } else {
                clients_message_queues.insert(client_key, vec![canister_to_client_message]);
            }
        },
    }
    // TODO: check without panicking
    // assert_eq!(*message_nonce, last_message_nonce); // check that messages are relayed in increasing order

    *message_nonce = last_message_nonce + 1;
    Ok(())
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

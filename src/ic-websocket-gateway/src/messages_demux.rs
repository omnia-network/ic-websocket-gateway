use std::collections::HashMap;

use candid::Principal;
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, warn};

use crate::{
    canister_methods::{
        CanisterOutputCertifiedMessages, CanisterOutputMessage, CanisterToClientMessage,
        ClientPrincipal,
    },
    canister_poller::{get_nonce_from_message, IcWsConnectionUpdate, PollerChannelsPollerEnds},
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
        clients_message_queues: &mut HashMap<Principal, Vec<CanisterToClientMessage>>,
        client_channels: &HashMap<ClientPrincipal, Sender<IcWsConnectionUpdate>>,
        poller_channels: &mut PollerChannelsPollerEnds,
        message_nonce: &mut u64,
    ) -> Result<(), String> {
        for canister_output_message in msgs.messages {
            if let Err(e) = relay_message(
                canister_output_message,
                msgs.cert.clone(),
                msgs.tree.clone(),
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
    // TODO: check without panicking
    // assert_eq!(*message_nonce, last_message_nonce); // check that messages are relayed in increasing order

    *message_nonce = last_message_nonce + 1;
    Ok(())
}

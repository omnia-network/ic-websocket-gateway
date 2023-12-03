use crate::{
    canister_methods::{
        self, CanisterOutputCertifiedMessages, CanisterToClientMessage,
        CanisterWsGetMessagesArguments, IcError,
    },
    events_analyzer::Events,
    manager::{CanisterPrincipal, ClientSender, GatewaySharedState, PollerState},
};
use candid::Principal;
use ic_agent::{Agent, AgentError};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc::Sender, time::Instant};
use tracing::{debug, error, info, span, trace, warn, Id, Instrument, Level, Span};

// OLD NOTE: 30 seems to be a good value for polling interval 100 ms and incoming connection rate up to 10 per second
//           as not so many polling iterations are idle and the effective polling interval (measured by PollerEventsMetrics) is mostly in [200, 300] ms
// TODO: make sure this is always in sync with the CDK init parameter 'max_number_of_returned_messages'
//       maybe get it once starting to poll the canister (?)
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 30;

enum PollingStatus {
    NoMessagesPolled,
    MessagesPolled(CanisterOutputCertifiedMessages),
    MaxMessagesPolled(CanisterOutputCertifiedMessages),
}

enum PollerStatus {
    Running,
    Terminated,
}

/// Canister message to be relayed to the client, together with its span
pub type IcWsCanisterMessage = (CanisterToClientMessage, Span);

/// Poller which periodically queries a canister for new messages and relays them to the client
pub struct CanisterPoller {
    /// Agent used to communicate with the IC
    agent: Arc<Agent>,
    /// Principal of the canister which the poller is polling
    canister_id: CanisterPrincipal,
    /// State of the poller
    poller_state: PollerState,
    /// State of the gateway
    gateway_shared_state: GatewaySharedState,
    /// Nonce specified by the gateway during the query call to ws_get_messages,
    /// used by the CDK to determine which messages to respond with
    message_nonce: u64,
    /// The number of polling iterations since the poller started
    /// reference of the PollerEvents
    polling_iteration: u64,
    /// Polling interval in milliseconds
    polling_interval_ms: u64,
    /// Sender side of the channel used to send events to the analyzer
    _analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
}

impl CanisterPoller {
    pub fn new(
        agent: Arc<Agent>,
        canister_id: Principal,
        poller_state: PollerState,
        gateway_shared_state: GatewaySharedState,
        polling_interval_ms: u64,
        _analyzer_channel_tx: Sender<Box<dyn Events + Send>>,
    ) -> Self {
        Self {
            agent,
            canister_id,
            poller_state,
            gateway_shared_state,
            // once the poller starts running, it requests messages from nonce 0.
            // if the canister already has some messages in the queue and receives the nonce 0, it knows that the poller restarted
            // therefore, it sends the last X messages to the gateway. From these, the gateway has to determine the response corresponding to the client's ws_open request
            // TODO: change the CDK so that in this case it returns all the messages in the queue
            //       the poller relays only the ones that are in the poller state at the time of receiving them
            message_nonce: 0,
            polling_iteration: 0,
            polling_interval_ms,
            _analyzer_channel_tx,
        }
    }

    /// Periodically polls the canister for updates to be relayed to clients
    pub async fn run_polling(&mut self) {
        loop {
            let polling_iteration_span = span!(Level::TRACE, "Polling Iteration", canister_id = %self.canister_id, polling_iteration = self.polling_iteration);
            if let Err(e) = self
                .poll_and_relay()
                .instrument(polling_iteration_span)
                .await
            {
                error!("Error polling canister: {:?}", e);
                // upon poller error, remove the poller state from the gateway state and immediately terminate the poller
                // client sessions will detect that the poller side of the channel has been dropped and therefore will also terminate
                // as the poller state contains all the state of the clients sessions opened to the failed poller, removing the poller state
                // will also remove all the corresponding clients' states
                // therefore, there is no need to wait for the clients to remove their state before terminating the poller
                self.gateway_shared_state
                    .remove_failed_canister(self.canister_id);
                // TODO: notify the canister that it cannot be polled anymore
                break;
            }

            // counting all polling iterations (instead of only the ones that return at least one canister message)
            // this way we can tell for how many iterations the poller was "idle" before actually getting some messages from the canister
            // this can help us in the future understanding whether the poller is polling too frequently or not
            self.polling_iteration += 1;

            if let PollerStatus::Terminated = self.check_poller_termination() {
                // the poller has been terminated
                break;
            }
        }
    }

    async fn poll_and_relay(&mut self) -> Result<(), String> {
        let relay_messages_span =
            span!(parent: &Span::current(), Level::TRACE, "Relay Canister Messages");

        let start_polling_instant: tokio::time::Instant = tokio::time::Instant::now();
        match self.poll_canister().await? {
            PollingStatus::NoMessagesPolled => {
                // if no messages are returned, sleep for 'polling_interval_ms' before polling again
                tokio::time::sleep(Duration::from_millis(self.polling_interval_ms)).await;
                Ok(())
            },
            PollingStatus::MessagesPolled(certified_canister_output) => {
                self.update_nonce(&certified_canister_output)?;
                let end_of_queue_reached = {
                    match certified_canister_output.is_end_of_queue {
                        Some(is_end_of_queue_reached) => is_end_of_queue_reached,
                        // if 'is_end_of_queue' is None, the CDK version is < 0.3.1 and does not have such a field
                        // in this case, assume that the queue is fully drained and therefore will be polled again
                        // after waiting for 'polling_interval_ms'
                        None => true,
                    }
                };
                relay_messages(Arc::clone(&self.poller_state), certified_canister_output)
                    .instrument(relay_messages_span)
                    .await?;
                let elapsed = get_elapsed(start_polling_instant);
                let polling_interval = Duration::from_millis(self.polling_interval_ms);
                // check if polling and relaying took longer than 'polling_interval'
                // if yes, restart polling immediately
                // check if the canister signaled that there are more messages in the queue
                // if yes, restart polling immediately
                // otherwise, sleep for the amount of time remaining to 'polling_interval'
                if elapsed > polling_interval {
                    warn!(
                        "Polling and relaying of messages took too long: {:?}. Polling immediately",
                        elapsed
                    );
                } else if !end_of_queue_reached {
                    warn!("Canister queue is not fully drained. Polling immediately");
                } else {
                    // SAFETY:
                    // 'elapsed' is smaller than 'polling_interval'
                    // therefore, the duration passed to 'sleep' is valid

                    // 'elapsed' is >= 0
                    // therefore, the next polling iteration is delayed by at most 'polling_interval'
                    tokio::time::sleep(polling_interval - elapsed).await;
                }
                Ok(())
            },
            PollingStatus::MaxMessagesPolled(certified_canister_output) => {
                self.update_nonce(&certified_canister_output)?;

                relay_messages(Arc::clone(&self.poller_state), certified_canister_output)
                    .instrument(relay_messages_span)
                    .await?;
                let elapsed = get_elapsed(start_polling_instant);
                // if polling and relaying took longer than 'polling_interval', create a warning
                if elapsed > Duration::from_millis(self.polling_interval_ms) {
                    warn!(
                        "Polling and relaying of messages took too long: {:?}",
                        elapsed
                    );
                }
                // poll immediately as the maximum number of messages has beeen polled
                warn!("Polled the maximum number of messages. Polling immediately");
                Ok(())
            },
        }
    }

    fn update_nonce(
        &mut self,
        certified_canister_output: &CanisterOutputCertifiedMessages,
    ) -> Result<(), String> {
        for canister_to_client_message in &certified_canister_output.messages {
            let last_message_nonce = get_nonce_from_message(&canister_to_client_message.key)?;
            self.message_nonce = last_message_nonce + 1;
        }
        Ok(())
    }

    /// Polls the canister for messages
    async fn poll_canister(&mut self) -> Result<PollingStatus, String> {
        trace!("Started polling iteration");

        // get messages to be relayed to clients from canister (starting from 'message_nonce')
        match canister_methods::ws_get_messages(
            &self.agent,
            &self.canister_id,
            CanisterWsGetMessagesArguments {
                nonce: self.message_nonce,
            },
        )
        .await
        {
            Ok(certified_canister_output) => {
                let number_of_polled_messages = certified_canister_output.messages.len();

                if number_of_polled_messages == 0 {
                    trace!("No messages polled from canister");
                    Ok(PollingStatus::NoMessagesPolled)
                } else if number_of_polled_messages >= MAX_NUMBER_OF_RETURNED_MESSAGES {
                    trace!("Polled the maximum number of messages");
                    Ok(PollingStatus::MaxMessagesPolled(certified_canister_output))
                } else {
                    trace!(
                        "Polled {} messages from canister",
                        number_of_polled_messages
                    );
                    Ok(PollingStatus::MessagesPolled(certified_canister_output))
                }
            },
            Err(IcError::Agent(e)) => {
                if is_recoverable_error(&e) {
                    // if the error is due to a replica which is either actively malicious or simply unavailable
                    // or to a malfunctioning boundary node,
                    // continue polling the canister as this is expected and other replicas might still be able to
                    // provide the canister updates
                    // TODO: add counter as after several retries the poller should be stopped
                    warn!("Ignoring replica error: {:?}", e);
                    Ok(PollingStatus::NoMessagesPolled)
                } else {
                    Err(format!("Unrecoverable agent error: {:?}", e))
                }
            },
            Err(IcError::Candid(e)) => Err(format!("Unrecoverable candid error: {:?}", e)),
            Err(IcError::Cdk(e)) => Err(format!("Unrecoverable CDK error: {:?}", e)),
        }
    }

    fn check_poller_termination(&mut self) -> PollerStatus {
        // check if the poller should be terminated
        // the poller does not necessarily need to be terminated as soon as the last client disconnects
        // therefore, we do not need to check if the poller state is empty in every single polling iteration
        if self.polling_iteration % 100 == 0 {
            if self
                .gateway_shared_state
                .remove_canister_if_empty(self.canister_id)
            {
                info!("Terminating poller");
                return PollerStatus::Terminated;
            }
        }
        PollerStatus::Running
    }
}

async fn relay_messages(
    poller_state: PollerState,
    msgs: CanisterOutputCertifiedMessages,
) -> Result<(), String> {
    for canister_output_message in msgs.messages {
        let canister_to_client_message = CanisterToClientMessage {
            key: canister_output_message.key,
            content: canister_output_message.content,
            cert: msgs.cert.clone(),
            tree: msgs.tree.clone(),
        };

        // TODO: figure out if keeping references to a value in the poller state can cause deadlocks
        if let Some(ClientSender {
            sender: client_channel_tx,
            span: client_session_span,
        }) = poller_state
            .get(&canister_output_message.client_key)
            .as_deref()
        {
            let canister_message_span = span!(parent: client_session_span, Level::TRACE, "Canister Message", message_key = canister_to_client_message.key);
            canister_message_span.follows_from(Span::current().id());
            let canister_update = canister_message_span.in_scope(|| {
                trace!("Received message from canister",);
                (canister_to_client_message, Span::current())
            });
            relay_message(canister_update, client_channel_tx)
                .instrument(canister_message_span)
                .await;
        } else {
            // SAFETY:
            // messages received from a client key that is not in the poller state are ignored
            // this is safe to do because we the client session handler relayes the messages to the IC
            // only after updating the poller state
            trace!("Polled message for a client that is not in the poller state anymore. Ignoring message");
        }
    }
    trace!("Relayed messages to connection handlers");
    Ok(())
}

async fn relay_message(
    canister_update: IcWsCanisterMessage,
    client_channel_tx: &Sender<IcWsCanisterMessage>,
) {
    if let Err(e) = client_channel_tx.send(canister_update).await {
        // SAFETY:
        // no need to panic here as the client session handler might have terminated
        // after the poller got the client_chanel_tx
        // the client session handler also updated the poller state so the poller can simply ignore this message
        warn!("Client's session terminated: {}", e);
    } else {
        trace!("Message relayed to connection handler");
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

fn get_elapsed(start: Instant) -> Duration {
    let now = Instant::now();
    now - start
}

fn is_recoverable_error(e: &AgentError) -> bool {
    match e {
        // TODO: make sure that we include all the "recoverable" errors
        AgentError::InvalidReplicaUrl(_)
        | AgentError::TimeoutWaitingForResponse()
        | AgentError::InvalidCborData(_)
        | AgentError::ReplicaError(_)
        | AgentError::HttpError(_)
        | AgentError::InvalidReplicaStatus
        | AgentError::RequestStatusDoneNoReply(_)
        | AgentError::LookupPathAbsent(_)
        | AgentError::LookupPathUnknown(_)
        | AgentError::LookupPathError(_)
        | AgentError::CertificateVerificationFailed()
        | AgentError::CertificateNotAuthorized()
        | AgentError::ResponseSizeExceededLimit()
        | AgentError::TransportError(_)
        | AgentError::CallDataMismatch { .. }
        | AgentError::InvalidRejectCode(_) => true,
        _ => false,
    }
}

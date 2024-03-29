use candid::Principal;
use canister_utils::{
    ws_get_messages, CanisterOutputCertifiedMessages, CanisterToClientMessage,
    CanisterWsGetMessagesArguments, IcError, IcWsCanisterMessage,
};
use gateway_state::{
    CanisterPrincipal, CanisterRemovalResult, ClientSender, GatewayState, PollerState,
};
use ic_agent::{agent::RejectCode, Agent, AgentError};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc::Sender, time::timeout};
use tracing::{error, span, trace, warn, Instrument, Level, Span};

pub(crate) const POLLING_TIMEOUT_MS: u64 = 5_000;

type PollingTimeout = Duration;

/// Result of the polling iteration
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PollingStatus {
    /// No messages polled
    NoMessagesPolled,
    /// Some messages polled
    MessagesPolled(CanisterOutputCertifiedMessages),
    /// Request timed out
    TimedOut,
}

/// Poller which periodically queries a canister for new messages and relays them to the client
pub struct CanisterPoller {
    /// Agent used to communicate with the IC
    agent: Arc<Agent>,
    /// Principal of the canister which the poller is polling
    canister_id: CanisterPrincipal,
    /// State of the poller
    poller_state: PollerState,
    /// State of the gateway
    gateway_state: GatewayState,
    /// Nonce specified by the gateway during the query call to ws_get_messages,
    /// used by the CDK to determine which messages to respond with
    next_message_nonce: u64,
    /// The number of polling iterations since the poller started
    /// reference of the PollerEvents
    polling_iteration: u64,
    /// Polling interval in milliseconds
    polling_interval_ms: u64,
}

impl CanisterPoller {
    pub fn new(
        agent: Arc<Agent>,
        canister_id: Principal,
        poller_state: PollerState,
        gateway_state: GatewayState,
        polling_interval_ms: u64,
    ) -> Self {
        Self {
            agent,
            canister_id,
            poller_state,
            gateway_state,
            next_message_nonce: 0,
            polling_iteration: 0,
            polling_interval_ms,
        }
    }

    /// Periodically polls the canister for updates to be relayed to clients
    pub async fn run_polling(&mut self) -> Result<(), String> {
        // keeps track of the previous polling iteration span in order to create a follow from relationship
        // initially set to None as the first iteration will not have a previous span
        let mut previous_polling_iteration_span: Option<Span> = None;
        loop {
            let polling_iteration_span = span!(Level::TRACE, "Polling Iteration", canister_id = %self.canister_id, polling_iteration = self.polling_iteration, cargo_version = env!("CARGO_PKG_VERSION"));
            if let Some(previous_polling_iteration_span) = previous_polling_iteration_span {
                // create a follow from relationship between the current and previous polling iteration
                // this enables to crawl polling iterations in reverse chronological order
                polling_iteration_span.follows_from(previous_polling_iteration_span.id());
            }
            if let Err(e) = self
                .poll_and_relay()
                .instrument(polling_iteration_span.clone())
                .await
            {
                error!("Error polling canister: {:?}", e);
                // upon poller error, remove the poller state from the gateway state and immediately terminate the poller
                // client sessions will detect that the poller side of the channel has been dropped and therefore will also terminate
                // as the poller state contains all the state of the clients sessions opened to the failed poller, removing the poller state
                // will also remove all the corresponding clients' states
                // therefore, there is no need to wait for the clients to remove their state before terminating the poller
                self.gateway_state.remove_failed_canister(self.canister_id);
                // TODO: notify the canister that it cannot be polled anymore
                return Err(e);
            }

            if self.poller_should_terminate() {
                // the poller has been terminated
                return Ok(());
            }

            // counting all polling iterations (instead of only the ones that return at least one canister message)
            // this way we can tell for how many iterations the poller was "idle" before actually getting some messages from the canister
            // this can help us in the future understanding whether the poller is polling too frequently or not
            self.polling_iteration += 1;
            previous_polling_iteration_span = Some(polling_iteration_span);
        }
    }

    pub async fn poll_and_relay(&mut self) -> Result<(), String> {
        let start_polling_instant = tokio::time::Instant::now();

        match self.poll_canister().await? {
            PollingStatus::MessagesPolled(certified_canister_output) => {
                let relay_messages_span =
                    span!(parent: &Span::current(), Level::TRACE, "Relay Canister Messages");
                // if 'is_end_of_queue' is None, the CDK version is < 0.3.1 and does not have such a field
                // in this case, assume that the queue is fully drained and therefore will be polled again
                // after waiting for 'polling_interval_ms'
                let end_of_queue_reached =
                    certified_canister_output.is_end_of_queue.unwrap_or(true);
                self.update_nonce(&certified_canister_output)?;
                // relaying of messages cannot be done in a separate task for each polling iteration
                // as they might interleave and break the correct ordering of messages
                // TODO: create a separate task dedicated to relaying messages which receives the messages from the poller via a queue
                //       and relays them in FIFO order
                self.relay_messages(certified_canister_output)
                    .instrument(relay_messages_span)
                    .await;
                if !end_of_queue_reached {
                    // if the queue is not fully drained, return immediately so that the next polling iteration can be started
                    warn!("Canister queue is not fully drained. Polling immediately");
                    return Ok(());
                }
            },
            PollingStatus::TimedOut => {
                // if the poller timed out, it already waited way too long... return immediately so that the next polling iteration can be started
                warn!("Poller timed out. Polling immediately");
                return Ok(());
            },
            PollingStatus::NoMessagesPolled => (),
        }

        // compute the amout of time to sleep for before polling again
        let effective_polling_interval =
            self.compute_effective_polling_interval(start_polling_instant);
        // if no messages are returned or if the queue is fully drained, sleep for 'effective_polling_interval' before polling again
        tokio::time::sleep(effective_polling_interval).await;
        Ok(())
    }

    /// Polls the canister for messages
    pub(crate) async fn poll_canister(&mut self) -> Result<PollingStatus, String> {
        trace!("Started polling iteration");

        // get messages to be relayed to clients from canister (starting from 'message_nonce')
        // the response timeout of the IC CDK is 2 minutes which implies that the poller would be stuck for that long waiting for a response
        // to prevent this, we set a timeout of 5 seconds, if the poller does not receive a response in time, it polls immediately
        // in case of a timeout, the message nonce is not updated so that no messages are lost by polling immediately again
        match timeout(
            PollingTimeout::from_millis(POLLING_TIMEOUT_MS),
            ws_get_messages(
                &self.agent,
                &self.canister_id,
                CanisterWsGetMessagesArguments {
                    nonce: self.next_message_nonce,
                },
            ),
        )
        .await
        {
            Ok(Ok(certified_canister_output)) => {
                let number_of_polled_messages = certified_canister_output.messages.len();
                if number_of_polled_messages == 0 {
                    trace!("No messages polled from canister");
                    Ok(PollingStatus::NoMessagesPolled)
                } else {
                    trace!(
                        "Polled {} messages from canister",
                        number_of_polled_messages
                    );
                    Ok(PollingStatus::MessagesPolled(certified_canister_output))
                }
            },
            Ok(Err(IcError::Agent(e))) => {
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
            Ok(Err(IcError::Candid(e))) => Err(format!("Unrecoverable candid error: {:?}", e)),
            Ok(Err(IcError::Cdk(e))) => Err(format!("Unrecoverable CDK error: {:?}", e)),
            Err(e) => {
                warn!("Poller took too long to retrieve messages: {:?}", e);
                Ok(PollingStatus::TimedOut)
            },
        }
    }

    async fn relay_messages(&self, msgs: CanisterOutputCertifiedMessages) {
        trace!("Started relaying messages");
        let mut relayed_messages_count = 0;
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
            }) = self
                .poller_state
                .get(&canister_output_message.client_key)
                .as_deref()
            {
                let canister_message_span = span!(parent: client_session_span, Level::TRACE, "Canister Message", message_key = canister_to_client_message.key, %self.canister_id);
                canister_message_span.follows_from(Span::current().id());
                let canister_message = canister_message_span.in_scope(|| {
                    trace!("Start relaying message",);
                    (canister_to_client_message, Span::current())
                });
                relay_message(canister_message, client_channel_tx)
                    .instrument(canister_message_span)
                    .await;
                relayed_messages_count += 1;
            }
            // SAFETY:
            // messages received from a client key that is not in the poller state are ignored
            // this is safe to do because we the client session handler relayes the messages to the IC
            // only after updating the poller state
        }
        trace!(
            "Relayed {} messages to connection handlers. The others were ignored",
            relayed_messages_count
        );
    }

    /// Computes the effective polling interval based on the time it took to poll the canister
    fn compute_effective_polling_interval(
        &self,
        start_polling_instant: tokio::time::Instant,
    ) -> Duration {
        let elapsed = tokio::time::Instant::now() - start_polling_instant;
        let polling_interval = Duration::from_millis(self.polling_interval_ms);
        // check if polling took longer than 'polling_interval'
        // if yes, restart polling immediately
        // otherwise, sleep for the amount of time remaining to 'polling_interval'
        if elapsed > polling_interval {
            warn!(
                "Polling messages took too long: {:?}. Polling immediately",
                elapsed
            );
            Duration::from_millis(0)
        } else {
            // SAFETY:
            // 'elapsed' is smaller than 'polling_interval'
            // therefore, the duration passed to 'sleep' is valid

            // 'elapsed' is >= 0
            // therefore, the next polling iteration is delayed by at most 'polling_interval'
            let effective_polling_interval = polling_interval - elapsed;
            trace!("Polling again in: {:?}", effective_polling_interval);
            effective_polling_interval
        }
    }

    /// Updates the message nonce according to the last polled message
    /// This is necessary to do before starting the next polling iteration
    /// Returns an error if a nonce could not be parsed from a message
    /// or if the polled nonces are not consecutive
    fn update_nonce(
        &mut self,
        certified_canister_output: &CanisterOutputCertifiedMessages,
    ) -> Result<(), String> {
        for canister_to_client_message in &certified_canister_output.messages {
            let message_nonce = get_nonce_from_message(&canister_to_client_message.key)?;
            // the first time the poller is started 'self.next_message_nonce' is 0
            // however, the canister might have already deleted messages that were relayed by previous pollers
            // therefore, if 'self.next_message_nonce' is 0 we can ignore the possible mismatch with the nonce of the first message received from the canister
            // all the other messages, instead shall arrive in the expected order (monotonically increasing and each adjacent to the next)
            if self.next_message_nonce != 0 && message_nonce != self.next_message_nonce {
                return Err(format!(
                    "Non consecutive nonce: expected {}, got {}",
                    self.next_message_nonce, message_nonce
                ));
            }
            self.next_message_nonce = message_nonce + 1;
        }
        Ok(())
    }

    fn poller_should_terminate(&mut self) -> bool {
        // check if the poller should be terminated
        // the poller needs to be terminated as soon as there are no more clients connected to the canister
        // this is necessary because the canister state might be cleaned immediately after the last client disconnects
        // if the poller state is not removed, a new client might connect to the poller before it is cleaned up and therefore
        // the poller would continue to poll from the current nonce
        // however, the canister state has been cleaned up and re-initialized after the client connected and so the new messages
        // will have a nonce starting from 0
        // this might cause the poller to not be able to poll the messages from the canister until they reach the current nonce
        // this is not acceptable as it results in messages being lost
        // therefore, check if the poller should be terminated after each polling iteration
        // TODO: find a more efficient way to do this, e.g. when the last client disconnects
        match self
            .gateway_state
            .remove_canister_if_empty(self.canister_id)
        {
            CanisterRemovalResult::Empty => true,
            CanisterRemovalResult::NotEmpty => false,
        }
    }
}

async fn relay_message(
    canister_message: IcWsCanisterMessage,
    client_channel_tx: &Sender<IcWsCanisterMessage>,
) {
    if let Err(e) = client_channel_tx.send(canister_message).await {
        // SAFETY:
        // no need to panic here as the client session handler might have terminated
        // after the poller got the client_chanel_tx
        // the client session handler also updated the poller state so the poller can simply ignore this message
        warn!("Client's session terminated: {}", e);
    } else {
        trace!("Message relayed to connection handler");
    }
}

pub fn get_nonce_from_message(key: &str) -> Result<u64, String> {
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

/// Returns true if the error is caused by a replica which is either actively malicious or simply unavailable
fn is_recoverable_error(e: &AgentError) -> bool {
    match e {
        // TODO: make sure that we include all the "recoverable" errors
        AgentError::InvalidReplicaUrl(_)
        | AgentError::TimeoutWaitingForResponse()
        | AgentError::InvalidCborData(_)
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
        // in case of a replica error, we recover only if the error is transient
        // all other errors (SysFatal, DestinationInvalid, CanisterReject, CanisterError) are considered permanent
        AgentError::ReplicaError(e) => e.reject_code == RejectCode::SysTransient,
        _ => false,
    }
}

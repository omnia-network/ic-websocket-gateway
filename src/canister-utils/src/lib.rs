use candid::{CandidType, Decode, Error, Principal};
use ic_agent::AgentError;
use ic_agent::{agent::http_transport::ReqwestTransport, identity::BasicIdentity, Agent};
use serde::{Deserialize, Serialize};
use std::fmt;
use tracing::Span;

static IC_MAINNET_URLS: [&str; 2] = ["https://icp0.io", "https://icp-api.io"];

pub type ClientPrincipal = Principal;

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug, Hash)]
pub struct ClientKey {
    pub client_principal: ClientPrincipal,
    pub client_nonce: u64,
}

impl ClientKey {
    /// Creates a new instance of ClientKey.
    pub fn new(client_principal: ClientPrincipal, client_nonce: u64) -> Self {
        Self {
            client_principal,
            client_nonce,
        }
    }
}

impl fmt::Display for ClientKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}_{}", self.client_principal, self.client_nonce)
    }
}

#[derive(Debug)]
pub enum IcError {
    Agent(AgentError),
    Candid(Error),
    Cdk(String),
}

/// The result of [ws_close].
pub type _CanisterWsCloseResult = Result<(), String>;
/// The result of [ws_get_messages].
pub type CanisterWsGetMessagesResultWithIcError = Result<CanisterOutputCertifiedMessages, IcError>;
/// The result of the canister method 'ws_get_messages'.
pub type CanisterWsGetMessagesResult = Result<CanisterOutputCertifiedMessages, String>;

/// The arguments for the canister method 'ws_open'.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsOpenArguments {
    pub client_nonce: u64,
    pub gateway_principal: Principal,
}

/// The arguments for [ws_close].
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsCloseArguments {
    pub client_key: ClientKey,
}

/// The arguments for [ws_get_messages].
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsGetMessagesArguments {
    pub nonce: u64,
}

/// Message polled from the canister
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

/// Messages exchanged through the WebSocket.
#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct WebsocketMessage {
    /// The client that the gateway will forward the message to or that sent the message.
    pub client_key: ClientKey,
    /// Both ways, messages should arrive with sequence numbers 0, 1, 2...
    pub sequence_num: u64,
    /// Timestamp of when the message was made for the recipient to inspect.
    pub timestamp: u64,
    /// Whether the message is a service message sent by the CDK to the client or vice versa.
    pub is_service_message: bool,
    /// Application message encoded in binary.
    #[serde(with = "serde_bytes")]
    pub content: Vec<u8>,
}

/// Element of the list of messages returned to the WS Gateway after polling.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct CanisterOutputMessage {
    /// The client that the gateway will forward the message to or that sent the message.
    pub client_key: ClientKey,
    /// Key for certificate verification.
    pub key: String,
    /// The message to be relayed, that contains the application message of type WesocketMessage.
    #[serde(with = "serde_bytes")]
    pub content: Vec<u8>,
}

#[derive(CandidType, Deserialize, Serialize)]
pub struct CanisterOpenMessageContent {
    pub client_key: ClientKey,
}

#[derive(CandidType, Deserialize, Serialize)]
pub struct CanisterAckMessageContent {
    pub last_incoming_sequence_num: u64,
}

/// A service message sent by the CDK to the client.
#[derive(CandidType, Deserialize, Serialize)]
pub enum CanisterServiceMessage {
    OpenMessage(CanisterOpenMessageContent),
    AckMessage(CanisterAckMessageContent),
}

/// List of messages returned to the WS Gateway after polling.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct CanisterOutputCertifiedMessages {
    pub messages: Vec<CanisterOutputMessage>, // List of messages.
    #[serde(with = "serde_bytes")]
    /// cert+tree constitute the certificate for all returned messages
    pub cert: Vec<u8>,
    #[serde(with = "serde_bytes")]
    /// cert+tree constitute the certificate for all returned messages
    pub tree: Vec<u8>,
    /// Flag set by the canister CDK to indicate if the messages polled are the last in the queue
    /// If true, the GW polls after waiting for 'polling_interval'
    /// If false, the GW polls immediately
    /// Wrapped in an Option because the canister CDK versions < 0.3.1 did not have this field
    /// When interacting with the canister CDK versions < 0.3.1, 'is_end_of_queue' is 'None'
    /// and the GW will apply the same logic as if it were 'Some(true)'
    pub is_end_of_queue: Option<bool>,
}

/// Canister message to be relayed to the client, together with its span
pub type IcWsCanisterMessage = (CanisterToClientMessage, Span);

pub fn is_mainnet(ic_network_url: &str) -> bool {
    IC_MAINNET_URLS.contains(&ic_network_url)
}

pub async fn get_new_agent(
    ic_network_url: &str,
    identity: BasicIdentity,
) -> Result<Agent, AgentError> {
    let is_mainnet = is_mainnet(ic_network_url);
    let transport = ReqwestTransport::create(ic_network_url.to_string())?;
    let agent = Agent::builder()
        .with_transport(transport)
        .with_identity(identity)
        .with_verify_query_signatures(is_mainnet)
        .build()?;
    if !is_mainnet {
        agent.fetch_root_key().await?;
    }
    Ok(agent)
}

pub async fn ws_close(
    agent: &Agent,
    canister_id: &Principal,
    args: CanisterWsCloseArguments,
) -> _CanisterWsCloseResult {
    let args = candid::encode_args((args,)).map_err(|e| e.to_string())?;

    let res = agent
        .update(canister_id, "ws_close")
        .with_arg(args)
        .call_and_wait()
        .await
        .map_err(|e| e.to_string())?;

    Decode!(&res, _CanisterWsCloseResult).map_err(|e| e.to_string())?
}

#[cfg(not(feature = "mock-server"))]
pub async fn ws_get_messages(
    agent: &Agent,
    canister_id: &Principal,
    args: CanisterWsGetMessagesArguments,
) -> CanisterWsGetMessagesResultWithIcError {
    let args = candid::encode_args((args,)).map_err(|e| IcError::Candid(e))?;

    let res = agent
        .query(canister_id, "ws_get_messages")
        .with_arg(args)
        .call()
        .await
        .map_err(|e| IcError::Agent(e))?;

    let res = Decode!(&res, CanisterWsGetMessagesResult).map_err(|e| IcError::Candid(e))?;
    res.map_err(|e| IcError::Cdk(e))
}

/// In order to call the mock server during testing, make sure that the 'mock-server'
/// feature is enabled when importing this crate as a dev-dependecy
/// e.g. canister-utils = { path = "../canister-utils", features = ["mock-server"] }
#[cfg(feature = "mock-server")]
pub async fn ws_get_messages(
    _agent: &Agent,
    _canister_id: &Principal,
    _args: CanisterWsGetMessagesArguments,
) -> CanisterWsGetMessagesResultWithIcError {
    // port must be set according to the one specified in MOCK_SERVER of tests/canister_poller.rs
    let res = reqwest::get("http://127.0.0.1:51558/ws_get_messages")
        .await
        .expect("Failed to make HTTP request")
        .bytes()
        .await
        .expect("Failed to read HTTP response");

    Decode!(&res, CanisterOutputCertifiedMessages).map_err(|e| IcError::Candid(e))
}

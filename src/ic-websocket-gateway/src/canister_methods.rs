use candid::{CandidType, Decode, Principal};
use ic_agent::AgentError;
use ic_agent::{
    agent::http_transport::ReqwestHttpReplicaV2Transport, identity::BasicIdentity, Agent,
};
use serde::{Deserialize, Serialize};
use std::fmt;

pub type ClientId = u64;

pub type ClientPrincipal = Principal;

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug, Hash)]
pub struct ClientKey {
    client_principal: ClientPrincipal,
    client_nonce: u64,
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

/// The result of [ws_close].
pub type CanisterWsCloseResult = Result<(), String>;
/// The result of [ws_get_messages].
pub type CanisterWsGetMessagesResult = Result<CanisterOutputCertifiedMessages, String>;

/// The arguments for [ws_open].
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
    pub cert: Vec<u8>, // cert+tree constitute the certificate for all returned messages.
    #[serde(with = "serde_bytes")]
    pub tree: Vec<u8>, // cert+tree constitute the certificate for all returned messages.
}

pub async fn get_new_agent(
    url: &str,
    identity: BasicIdentity,
    fetch_key: bool,
) -> Result<Agent, AgentError> {
    let transport = ReqwestHttpReplicaV2Transport::create(url.to_string())?;
    let agent = Agent::builder()
        .with_transport(transport)
        .with_identity(identity)
        .build()?;
    if fetch_key {
        agent.fetch_root_key().await?;
    }
    Ok(agent)
}

pub async fn ws_close(
    agent: &Agent,
    canister_id: &Principal,
    args: CanisterWsCloseArguments,
) -> CanisterWsCloseResult {
    let args = candid::encode_args((args,)).map_err(|e| e.to_string())?;

    let res = agent
        .update(canister_id, "ws_close")
        .with_arg(args)
        .call_and_wait()
        .await
        .map_err(|e| e.to_string())?;

    Decode!(&res, CanisterWsCloseResult).map_err(|e| e.to_string())?
}

pub async fn ws_get_messages(
    agent: &Agent,
    canister_id: &Principal,
    args: CanisterWsGetMessagesArguments,
) -> CanisterWsGetMessagesResult {
    let args = candid::encode_args((args,)).map_err(|e| e.to_string())?;

    let res = agent
        .query(canister_id, "ws_get_messages")
        .with_arg(args)
        .call()
        .await
        .map_err(|e| e.to_string())?;

    Decode!(&res, CanisterWsGetMessagesResult).map_err(|e| e.to_string())?
}

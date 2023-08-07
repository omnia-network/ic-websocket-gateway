use candid::CandidType;
use candid::Decode;
use ed25519_compact::{PublicKey, Signature};
use ic_agent::AgentError;
use ic_agent::{
    agent::http_transport::ReqwestHttpReplicaV2Transport, export::Principal,
    identity::BasicIdentity, Agent,
};
use serde::{Deserialize, Serialize};
use serde_cbor::from_slice;
use tokio_tungstenite::tungstenite::Message;

pub type ClientPublicKey = Vec<u8>;

/// The result of `ws_open`.
pub type CanisterWsOpenResult = Result<CanisterWsOpenResultValue, String>;
/// The result of `ws_message`.
pub type CanisterWsMessageResult = Result<(), String>;
/// The result of `ws_get_messages`.
pub type CanisterWsGetMessagesResult = Result<CanisterOutputCertifiedMessages, String>;
/// The result of `ws_close`.
pub type CanisterWsCloseResult = Result<(), String>;

/// The Ok value of CanisterWsOpenResult returned by `ws_open`
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsOpenResultValue {
    pub client_key: ClientPublicKey,
    pub canister_id: Principal,
    pub nonce: u64,
}

/// The arguments for `ws_register`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsRegisterArguments {
    #[serde(with = "serde_bytes")]
    client_key: ClientPublicKey,
}

/// The arguments for `ws_open`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsOpenArguments {
    #[serde(with = "serde_bytes")]
    msg: Vec<u8>,
    #[serde(with = "serde_bytes")]
    sig: Vec<u8>,
}

/// The arguments for `ws_close`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsCloseArguments {
    #[serde(with = "serde_bytes")]
    client_key: ClientPublicKey,
}

/// The arguments for `ws_message`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsMessageArguments {
    msg: CanisterIncomingMessage,
}

/// The arguments for `ws_get_messages`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsGetMessagesArguments {
    nonce: u64,
}

/// The first message received by the canister in `ws_open`.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterFirstMessageContent {
    #[serde(with = "serde_bytes")]
    pub client_key: ClientPublicKey,
    pub canister_id: Principal,
}

/// message + signature from client, **relayed** by the WS Gateway.
#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct RelayedClientMessage {
    #[serde(with = "serde_bytes")]
    pub content: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub sig: Vec<u8>,
}

/// Message coming directly from client, not relayed by the WS Gateway.
#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct DirectClientMessage {
    pub message: Vec<u8>,
    pub client_key: ClientPublicKey,
}

/// The possible messages received by the canister in `ws_message`.
#[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum CanisterIncomingMessage {
    DirectlyFromClient(DirectClientMessage),
    RelayedByGateway(RelayedClientMessage),
    IcWebSocketEstablished(ClientPublicKey),
    IcWebSocketGatewayStatus(usize),
}

/// Messages exchanged through the WebSocket.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct WebsocketMessage {
    #[serde(with = "serde_bytes")]
    pub client_key: ClientPublicKey, // To or from client key.
    pub sequence_num: u64, // Both ways, messages should arrive with sequence numbers 0, 1, 2...
    pub timestamp: u64,    // Timestamp of when the message was made for the recipient to inspect.
    #[serde(with = "serde_bytes")]
    pub message: Vec<u8>, // Application message encoded in binary.
}

/// Member of the list of messages returned to the polling WS Gateway.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct CanisterOutputMessage {
    #[serde(with = "serde_bytes")]
    pub client_key: ClientPublicKey, // The client that the gateway will forward the message to.
    pub key: String, // Key for certificate verification.
    #[serde(with = "serde_bytes")]
    pub val: Vec<u8>, // Encoded WebsocketMessage.
}

/// List of messages returned to the polling gateway.
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

fn validate_first_message(
    bytes: Vec<u8>,
) -> Result<(RelayedClientMessage, CanisterFirstMessageContent), String> {
    let m = from_slice::<RelayedClientMessage>(&bytes)
        .map_err(|_| String::from("first message is not of type RelayedClientMessage"))?;
    let content = from_slice::<CanisterFirstMessageContent>(&m.content).map_err(|_| {
        String::from("content of first message is not of type CanisterFirstMessageContent")
    })?;
    let sig = Signature::from_slice(&m.sig)
        .map_err(|_| String::from("first message does not contain a valid signature"))?;
    let public_key = PublicKey::from_slice(&content.client_key)
        .map_err(|_| String::from("first message does not contain a valid public key"))?;
    public_key
        .verify(&m.content, &sig)
        .map_err(|_| String::from("client's signature does not verify against public key"))?;
    Ok((m, content))
}

#[cfg(not(test))] // only compile and run the following block when not running tests
pub async fn check_canister_init(agent: &Agent, message: Message) -> CanisterWsOpenResult {
    if let Message::Binary(bytes) = message {
        let (m, content) = validate_first_message(bytes)?;
        // if all checks pass, call the ws_open method of the canister which the WS Gateway has to poll from
        ws_open(agent, &content.canister_id, m.content, m.sig).await
    } else {
        Err(String::from(
            "first message from client should be binary encoded",
        ))
    }
}

#[cfg(test)] // only compile and run the following block during tests
pub async fn check_canister_init(_agent: &Agent, message: Message) -> CanisterWsOpenResult {
    if let Message::Binary(bytes) = message {
        let (_m, _content) = validate_first_message(bytes)?;

        // mock the result returned by a call to ws_open after the client registered its public key
        let valid_client_key = vec![
            229, 173, 124, 88, 70, 98, 66, 88, 106, 214, 233, 97, 108, 15, 187, 54, 121, 43, 50,
            45, 131, 52, 17, 59, 72, 46, 186, 105, 141, 71, 119, 203,
        ];
        Ok(CanisterWsOpenResultValue {
            client_key: valid_client_key,
            canister_id: Principal::from_text("bkyz2-fmaaa-aaaaa-qaaaq-cai")
                .expect("not a valid principal"),
            nonce: 0,
        })
    } else {
        Err(String::from(
            "first message from client should be binary encoded",
        ))
    }
}

#[cfg(not(test))] // only compile and run the following block during tests
pub async fn ws_open(
    agent: &Agent,
    canister_id: &Principal,
    content: Vec<u8>,
    sig: Vec<u8>,
) -> CanisterWsOpenResult {
    let args = candid::encode_args((CanisterWsOpenArguments { msg: content, sig },))
        .map_err(|e| e.to_string())?;

    let res = agent
        .update(canister_id, "ws_open")
        .with_arg(args)
        .call_and_wait()
        .await
        .map_err(|e| e.to_string())?;

    Decode!(&res, CanisterWsOpenResult).map_err(|e| e.to_string())?
}

pub async fn ws_close(
    agent: &Agent,
    canister_id: &Principal,
    client_key: ClientPublicKey,
) -> CanisterWsCloseResult {
    let args = candid::encode_args((CanisterWsCloseArguments { client_key },))
        .map_err(|e| e.to_string())?;

    let res = agent
        .update(canister_id, "ws_close")
        .with_arg(args)
        .call_and_wait()
        .await
        .map_err(|e| e.to_string())?;

    Decode!(&res, CanisterWsCloseResult).map_err(|e| e.to_string())?
}

pub async fn ws_message(
    agent: &Agent,
    canister_id: &Principal,
    msg: CanisterIncomingMessage,
) -> CanisterWsMessageResult {
    let args =
        candid::encode_args((CanisterWsMessageArguments { msg },)).map_err(|e| e.to_string())?;

    let res = agent
        .update(canister_id, "ws_message")
        .with_arg(args)
        .call_and_wait()
        .await
        .map_err(|e| e.to_string())?;

    Decode!(&res, CanisterWsMessageResult).map_err(|e| e.to_string())?
}

pub async fn ws_get_messages(
    agent: &Agent,
    canister_id: &Principal,
    nonce: u64,
) -> CanisterWsGetMessagesResult {
    let args = candid::encode_args((CanisterWsGetMessagesArguments { nonce },))
        .map_err(|e| e.to_string())?;

    let res = agent
        .query(canister_id, "ws_get_messages")
        .with_arg(&args)
        .call()
        .await
        .map_err(|e| e.to_string())?;

    Decode!(&res, CanisterWsGetMessagesResult).map_err(|e| e.to_string())?
}

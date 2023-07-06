use candid::CandidType;
use candid::Decode;
use ed25519_compact::PublicKey;
use ic_agent::{
    agent::http_transport::ReqwestHttpReplicaV2Transport, export::Principal,
    identity::BasicIdentity, Agent,
};
use serde::{Deserialize, Serialize};

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[candid_path("ic_cdk::export::candid")]
pub struct WebsocketMessage {
    pub client_id: u64,
    pub sequence_num: u64,
    pub timestamp: u64,
    #[serde(with = "serde_bytes")]
    pub message: Vec<u8>,
}

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct EncodedMessage {
    pub client_id: u64,
    pub key: String,
    #[serde(with = "serde_bytes")]
    pub val: Vec<u8>,
}

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct CertMessages {
    pub messages: Vec<EncodedMessage>,
    #[serde(with = "serde_bytes")]
    pub cert: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub tree: Vec<u8>,
}

pub async fn get_new_agent(url: &str, identity: BasicIdentity, fetch_key: bool) -> Agent {
    let transport = ReqwestHttpReplicaV2Transport::create(url.to_string()).unwrap();
    let agent = Agent::builder()
        .with_transport(transport)
        .with_identity(identity)
        .build()
        .unwrap();
    if fetch_key {
        agent.fetch_root_key().await.unwrap();
    }
    agent
}

pub async fn ws_get_client_key(
    agent: &Agent,
    canister_id: &Principal,
    client_id: u64,
) -> PublicKey {
    let args = candid::encode_args((client_id,))
        .map_err(|e| e.to_string())
        .unwrap();

    let res = agent
        .query(canister_id, "ws_get_client_key")
        .with_arg(&args)
        .call()
        .await
        .unwrap();

    PublicKey::from_slice(&Decode!(&res, Vec<u8>).map_err(|e| e.to_string()).unwrap()).unwrap()
}

pub async fn ws_open(
    agent: &Agent,
    canister_id: &Principal,
    content: Vec<u8>,
    sig: Vec<u8>,
) -> bool {
    let args = candid::encode_args((content, sig)).unwrap();

    let res = agent
        .update(canister_id, "ws_open")
        .with_arg(args)
        .call_and_wait()
        .await
        .unwrap();

    Decode!(&res, bool).map_err(|e| e.to_string()).unwrap()
}

pub async fn ws_close(agent: &Agent, canister_id: &Principal, can_client_id: u64) {
    let args = candid::encode_args((can_client_id,)).unwrap();

    let res = agent
        .update(canister_id, "ws_close")
        .with_arg(args)
        .call_and_wait()
        .await
        .unwrap();

    Decode!(&res, ()).map_err(|e| e.to_string()).unwrap()
}

pub async fn ws_message(agent: &Agent, canister_id: &Principal, mes: Vec<u8>) -> bool {
    let args = candid::encode_args((mes,)).unwrap();

    let res = agent
        .update(canister_id, "ws_message")
        .with_arg(args)
        .call_and_wait()
        .await
        .unwrap();

    Decode!(&res, bool).map_err(|e| e.to_string()).unwrap()
}

pub async fn ws_get_messages(agent: &Agent, canister_id: &Principal, nonce: u64) -> CertMessages {
    let args = candid::encode_args((nonce,))
        .map_err(|e| e.to_string())
        .unwrap();

    let res = agent
        .query(canister_id, "ws_get_messages")
        .with_arg(&args)
        .call()
        .await
        .unwrap();

    Decode!(&res, CertMessages)
        .map_err(|e| e.to_string())
        .unwrap()
}

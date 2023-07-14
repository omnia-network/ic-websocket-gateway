use candid::CandidType;
use canister_methods::CertMessages;
use ed25519_compact::{PublicKey, Signature};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use ic_cdk::println;
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, to_vec};
use std::net::SocketAddr;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error, Message, Result},
};

mod canister_methods;

// url for local testing
// for local testing also the agent needs to fetch the root key
const URL: &str = "http://127.0.0.1:4943";
const FETCH_KEY: bool = true;

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
pub enum GatewayMessage {
    RelayedFromClient(MessageFromClient),
    FromGateway(Vec<u8>, bool),
}

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
pub struct MessageFromClient {
    #[serde(with = "serde_bytes")]
    content: Vec<u8>,
    #[serde(with = "serde_bytes")]
    sig: Vec<u8>,
}

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
struct ClientCanisterId {
    #[serde(with = "serde_bytes")]
    client_key: Vec<u8>,
    canister_id: Principal,
}

#[derive(Debug, Clone)]
struct GatewaySession {
    client_id: u64,
    client_key: Vec<u8>,
    canister_id: Principal,
    message_for_client_tx: UnboundedSender<CertMessage>,
}

#[derive(Debug)]
enum IcWsError {
    InitializationError(String), // error due to the client not following the IC WS initialization protocol
    WsError(Error),              // WebSocket error
    WsClose(String),             // WebSocket closed by client
}

async fn check_canister_init(
    agent: &Agent,
    client_addr: SocketAddr,
    message: Message,
) -> Result<(Vec<u8>, Principal), String> {
    if let Message::Binary(bytes) = message {
        let m = from_slice::<MessageFromClient>(&bytes)
            .map_err(|_| String::from("first message is not of type MessageFromClient"))?;
        let content = from_slice::<ClientCanisterId>(&m.content).map_err(|_| {
            String::from("content of first message is not of type ClientCanisterId")
        })?;
        let sig = Signature::from_slice(&m.sig)
            .map_err(|_| String::from("first message does not contain a valid signature"))?;
        let public_key = PublicKey::from_slice(&content.client_key)
            .map_err(|_| String::from("first message does not contain a valid public key"))?;
        public_key
            .verify(&m.content, &sig)
            .map_err(|_| String::from("client's signature does not verify against public key"))?;
        if canister_methods::ws_open(agent, &content.canister_id, m.content, m.sig).await {
            println!("New WebSocket connection: {}", client_addr);
            Ok((content.client_key, content.canister_id))
        } else {
            Err(String::from(
                "canister could not verify client's signature against public key",
            ))
        }
    } else {
        Err(String::from(
            "first message from client should be binary encoded",
        ))
    }
}

async fn handle_connection(
    client_id: u64,
    agent: &Agent,
    client_addr: SocketAddr,
    stream: TcpStream,
    connection_handler_tx: UnboundedSender<Result<GatewaySession, u64>>,
) -> Result<(), IcWsError> {
    match accept_async(stream).await {
        Ok(ws_stream) => {
            let (mut ws_write, mut ws_read) = ws_stream.split();
            let mut is_first_message = true;
            let (message_for_client_tx, mut message_for_client_rx) = mpsc::unbounded_channel();
            loop {
                select! {
                    msg_res = ws_read.try_next() => {
                        match msg_res {
                            Ok(Some(message)) => {
                                if message.is_close() {
                                    connection_handler_tx.send(
                                        Err(client_id)
                                    ).expect("channel should be open on the main thread");
                                    return Err(IcWsError::WsClose(format!("WebSocket stream has been closed by the client with id: {}", client_id)));
                                }
                                if is_first_message {
                                    // check if client correctly registered its public key in the backend canister
                                    match check_canister_init(agent, client_addr, message.clone()).await {
                                        Ok((client_key, canister_id)) => {
                                            // tell the client that the IC WS connection is setup correctly
                                            ws_write.send(Message::Text("1".to_string())).await.map_err(|e| {
                                                IcWsError::WsError(e)
                                            })?;

                                            let message_for_client_tx_cl = message_for_client_tx.clone();
                                            connection_handler_tx.send(
                                                Ok(
                                                    GatewaySession {
                                                        client_id,
                                                        client_key,
                                                        canister_id,
                                                        message_for_client_tx: message_for_client_tx_cl,
                                                    },
                                                )
                                            ).expect("channel should be open on the main thread");
                                        },
                                        Err(e) => {
                                            // tell the client that the setup of the IC WS connection failed
                                            ws_write.send(Message::Text("0".to_string())).await.map_err(|e| {
                                                IcWsError::WsError(e)
                                            })?;
                                            return Err(IcWsError::InitializationError(e));
                                        }
                                    };
                                    is_first_message = false;
                                }
                                else {
                                    // TODO: handle incoming message from client
                                    // println!("Client sent message: {:?}", message);
                                }
                            }
                            Ok(None) => return Err(IcWsError::WsError(Error::AlreadyClosed)),
                            Err(err) => return Err(IcWsError::WsError(err))
                        }
                    }
                    Some(message) = message_for_client_rx.recv() => {
                        // send canister message to client
                        ws_write.send(Message::Binary(to_vec(&message).unwrap())).await.map_err(|e| {
                            IcWsError::WsError(e)
                        })?;
                    }
                }
            }
        }
        Err(e) => return Err(IcWsError::WsError(e)),
    }
}

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct CertMessage {
    pub key: String,
    #[serde(with = "serde_bytes")]
    pub val: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub cert: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub tree: Vec<u8>,
}

struct CanisterPoller {
    canister_id: Principal,
    agent: Arc<Agent>,
}

impl CanisterPoller {
    async fn run_polling(&self, clients_updates_tx: UnboundedSender<CertMessages>) {
        let agent = Arc::clone(&self.agent);
        let canister_id = self.canister_id;
        tokio::spawn({
            let interval = Duration::from_millis(200);
            let mut nonce: u64 = 0;
            async move {
                loop {
                    let msgs =
                        canister_methods::ws_get_messages(&*agent, &canister_id, nonce).await;

                    if msgs.messages.len() > 0 {
                        // increment nonce in order to not request the same messages again
                        for encoded_message in msgs.messages.iter() {
                            nonce = encoded_message
                                .key
                                .split('_')
                                .last()
                                .unwrap()
                                .parse()
                                .unwrap();
                            nonce += 1
                        }
                        clients_updates_tx
                            .send(msgs)
                            .expect("channel should be open");
                    }

                    tokio::time::sleep(interval).await;
                }
            }
        });
    }
}

struct GatewayServer {
    connected_canisters: HashMap<Principal, CanisterPoller>,
    client_session_map: HashMap<Vec<u8>, GatewaySession>,
    client_key_map: HashMap<u64, Vec<u8>>,
}

#[tokio::main]
async fn main() {
    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(&addr).await.expect("Can't listen");
    println!("Listening on: {}", addr);

    let rng = ring::rand::SystemRandom::new();
    let key_pair = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
        .expect("Could not generate a key pair.");
    let identity = BasicIdentity::from_key_pair(
        ring::signature::Ed25519KeyPair::from_pkcs8(key_pair.as_ref())
            .expect("Could not read the key pair."),
    );
    let agent = Arc::new(canister_methods::get_new_agent(URL, identity, FETCH_KEY).await);
    agent.fetch_root_key().await.unwrap();

    let mut gateway_server = GatewayServer {
        connected_canisters: HashMap::default(),
        client_session_map: HashMap::default(),
        client_key_map: HashMap::default(),
    };

    let (connection_handler_tx, mut connection_handler_rx) = mpsc::unbounded_channel();
    let (clients_updates_tx, mut clients_updates_rx): (
        UnboundedSender<CertMessages>,
        UnboundedReceiver<CertMessages>,
    ) = mpsc::unbounded_channel();
    let agent_cl = Arc::clone(&agent);
    let client_id = Arc::new(Mutex::new(0)); // needed to know which gateway_session to delete in case of error or WS closed
                                             // spawn a task which keeps accepting and handling incoming connection requests from WebSocket clients
    tokio::spawn(async move {
        while let Ok((stream, client_addr)) = listener.accept().await {
            let agent_cl = Arc::clone(&agent_cl);
            let connection_handler_tx_cl = connection_handler_tx.clone();
            let client_id = Arc::clone(&client_id);
            // spawn a connection handler task for each incoming connection
            tokio::spawn(async move {
                let next_client_id = {
                    let mut next_client_id = client_id.lock().await;
                    *next_client_id += 1;
                    *next_client_id
                };
                println!("\nNew client id: {}", next_client_id);
                let end_connection_result = handle_connection(
                    next_client_id,
                    &*agent_cl,
                    client_addr,
                    stream,
                    connection_handler_tx_cl,
                )
                .await;
                println!("Client connection terminated: {:?}", end_connection_result);
            });
        }
    });

    loop {
        select! {
            Some(msgs) = clients_updates_rx.recv() => {
                // triggered when fetched a update messages from canister
                for encoded_message in msgs.messages {
                    let client_key = encoded_message.client_key;

                    println!(
                        "Message to client {:?} with key {}.",
                        client_key, encoded_message.key
                    );

                    let m = CertMessage {
                        key: encoded_message.key.clone(),
                        val: encoded_message.val,
                        cert: msgs.cert.clone(),
                        tree: msgs.tree.clone(),
                    };

                    match gateway_server.client_session_map.get(&client_key) {
                        Some(gateway_session) => {
                            if let Err(e) = gateway_session.message_for_client_tx.send(m) {
                                println!("Client's thread terminated: {}", e);
                            }
                        },
                        None => println!("Connection with client closed before message could be delivered")
                    }
                }
            }
            Some(connection_result) = connection_handler_rx.recv() => {
                match connection_result {
                    Ok(gateway_session) => {
                        // notify canister that it can now send messages for the client corresponding to client_key
                        let gateway_message = GatewayMessage::FromGateway(gateway_session.client_key.clone(), true);
                        canister_methods::ws_message(&*agent, &gateway_session.canister_id, gateway_message).await;

                        gateway_server.client_key_map.insert(gateway_session.client_id.clone(), gateway_session.client_key.clone());
                        gateway_server.client_session_map.insert(gateway_session.client_key.clone(), gateway_session.clone());
                        println!("{} clients registered", gateway_server.client_session_map.len());
                        if let false = gateway_server.connected_canisters.contains_key(&gateway_session.canister_id) {
                            let poller = CanisterPoller {
                                canister_id: gateway_session.canister_id.clone(),
                                agent: Arc::clone(&agent),
                            };
                            let clients_updates_tx_cl = clients_updates_tx.clone();
                            poller.run_polling(clients_updates_tx_cl).await;
                            println!("Created new poller for canister: {}", poller.canister_id);
                            gateway_server
                                .connected_canisters.insert(poller.canister_id, poller);
                        }
                    },
                    Err(client_id) => {
                        match gateway_server.client_key_map.remove(&client_id) {
                            Some(client_key) => {
                                let gateway_session = gateway_server.client_session_map.remove(&client_key).expect("gateway session should be registered");
                                canister_methods::ws_close(&*agent, &gateway_session.canister_id, client_key).await;
                                println!("{} clients registered", gateway_server.client_session_map.len());
                            },
                            None => {
                                println!("Client closed connection before being registered");
                            }
                        }
                    }
                }
            }
        }
    }
}

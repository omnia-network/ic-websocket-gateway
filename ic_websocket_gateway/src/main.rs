use futures_util::{StreamExt, SinkExt};
use std::net::SocketAddr;
use tokio::{net::{TcpListener, TcpStream}, sync::{RwLock, mpsc::{self, UnboundedSender}}, select};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error, Result, Message}, WebSocketStream,
};
use candid::CandidType;
use ed25519_compact::{Signature, PublicKey};
use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, to_vec};
use std::{
    collections::HashMap,
    sync::Arc,
    time::Duration,
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
    FromGateway(Vec<u8>, bool)
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
    client_key: Vec<u8>,
    canister_id: Principal,
}

#[derive(Debug)]
enum IcWsInitializationError {
    CustomError(String),    // error due to the client not following the IC WS initialization protocol
    WsError(Error),     // WebSocket error
}

async fn check_canister_init(agent: &Agent, client_addr: SocketAddr, message: Message) -> Result<(Vec<u8>, Principal), String> {
    if let Message::Binary(bytes) = message {
        let m = from_slice::<MessageFromClient>(&bytes).map_err(|_| {
            String::from("first message is not of type MessageFromClient")
        })?;
        let content = from_slice::<ClientCanisterId>(&m.content).map_err(|_| {
            String::from("content of first message is not of type ClientCanisterId")
        })?;
        let sig = Signature::from_slice(&m.sig).map_err(|_| {
            String::from("first message does not contain a valid signature")
        })?;
        let public_key = PublicKey::from_slice(&content.client_key).map_err(|_| {
            String::from("first message does not contain a valid public key")
        })?;
        public_key.verify(&m.content, &sig).map_err(|_| {
            String::from("client's signature does not verify against public key")
        })?;
        if canister_methods::ws_open(agent, &content.canister_id, m.content, m.sig).await {
            println!("New WebSocket connection: {}", client_addr);
            Ok((content.client_key, content.canister_id))
        }
        else {
            Err(String::from("canister could not verify client's signature against public key"))
        }
    }
    else {
        Err(String::from("first message from client should be binary encoded"))
    }
}

async fn handle_connection(agent: &Agent, client_addr: SocketAddr, stream: TcpStream, session_init_tx: UnboundedSender<Result<(GatewaySession, WebSocketStream<TcpStream>), IcWsInitializationError>>) {
    let connection_result = match accept_async(stream).await {
        Ok(mut ws_stream) => {
            if let Some(first_msg) = ws_stream.next().await {
                match first_msg {
                    Ok(msg) => {
                        // check if client correctly registered its public key in the backend canister
                        match check_canister_init(agent, client_addr, msg).await {
                            Ok((client_key, canister_id)) => {
                                ws_stream.send(Message::Text("1".to_string())).await;   // tell the client that the IC WS connection is setup correctly
                                Ok((
                                    GatewaySession {
                                        client_key,
                                        canister_id,
                                    },
                                    ws_stream
                                ))
                            },
                            Err(e) => {
                                ws_stream.send(Message::Text("0".to_string())).await;   // tell the client that the setup of the IC WS connection failed
                                Err(IcWsInitializationError::CustomError(e))
                            }
                        }
                    },
                    Err(e) => Err(IcWsInitializationError::WsError(e)),
                }
            }
            else {
                ws_stream.send(Message::Text("0".to_string())).await;   // tell the client that the setup of the IC WS connection failed
                Err(IcWsInitializationError::CustomError(String::from("client should send first message")))
            }
        },
        Err(e) => Err(IcWsInitializationError::WsError(e))
    };
    session_init_tx.send(connection_result).expect("channel should be open");
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
    canister_client_session_map: HashMap<Vec<u8>, GatewaySession>,
    agent: Arc<Agent>
}

impl CanisterPoller {
    async fn run_polling(&self, message_for_client_tx: UnboundedSender<(Vec<u8>, CertMessage)>) {
        let agent = Arc::clone(&self.agent);
        let canister_id = self.canister_id;
        tokio::spawn({
            let interval = Duration::from_millis(200);
            let mut nonce: u64 = 0;
            async move {
                loop {
                    let msgs = canister_methods::ws_get_messages(&*agent, &canister_id, nonce).await;

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

                        if let Err(e) = message_for_client_tx.send((client_key, m)) {
                            println!("Error sending canister message to main thread: {}", e);
                        }

                        nonce = encoded_message
                            .key
                            .split('_')
                            .last()
                            .unwrap()
                            .parse()
                            .unwrap();
                        nonce += 1
                    }

                    tokio::time::sleep(interval).await;
                }
            }
        });
    }

    fn add_session(&mut self, canister_client_key: Vec<u8>, session: GatewaySession) {
        self.canister_client_session_map.insert(canister_client_key, session);
    }
}

struct GatewayServer {
    connected_canisters: HashMap<Principal, CanisterPoller>,
    session_streamers: HashMap<Vec<u8>, WebSocketStream<TcpStream>>,
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

    let gateway_server = Arc::new(RwLock::new(GatewayServer {
        connected_canisters: HashMap::default(),
        session_streamers: HashMap::default(),
    }));

    let (session_init_tx, mut session_init_rx) = mpsc::unbounded_channel();
    let (message_for_client_tx, mut message_for_client_rx) = mpsc::unbounded_channel();
    let agent_cl = Arc::clone(&agent);
    // spawn a task which keeps accepting and handling incoming connection requests from WebSocket clients
    tokio::spawn(async move {
        while let Ok((stream, client_addr)) = listener.accept().await {
            let agent_cl = Arc::clone(&agent_cl);
            let session_init_tx_cl = session_init_tx.clone();
            // spawn a connection handler task for each incoming connection 
            tokio::spawn(async move {
                println!("\nClient address: {}", client_addr);
                handle_connection(&*agent_cl, client_addr, stream, session_init_tx_cl).await
            });
        }
    });

    loop {
        select! {
            // prioritize relaying updates to already connected clients rather than handling new client connections
            Some((client_key, message)) = message_for_client_rx.recv() => {
                match gateway_server.write().await.session_streamers.get_mut(&client_key) {
                    Some(ws_stream) => {
                        if let Err(e) = ws_stream.send(Message::Binary(to_vec(&message).unwrap())).await {
                            println!("WebSocket error: {}", e);
                        }
                    },
                    None => println!("No client with id: {:?}", client_key)
                }
            }
            Some(connection_result) = session_init_rx.recv() => {
                match connection_result {
                    Ok((gateway_session, ws_stream)) => {
                        // ensure that the lock is released before executing the match arms
                        // not doing so would result in a deadlock as the lock is awaited within each arm
                        // but it would not be released by the expression being matched until an arm is executed
                        let poller_is_initialized = {
                            gateway_server.write().await.session_streamers.insert(gateway_session.client_key.clone(), ws_stream);

                            // notify canister that it can now send messages for the client corresponding to client_key
                            let gateway_message = GatewayMessage::FromGateway(gateway_session.client_key.clone(), true);
                            canister_methods::ws_message(&*agent, &gateway_session.canister_id, gateway_message).await;

                            gateway_server.read().await.connected_canisters.contains_key(&gateway_session.canister_id)
                        };
                        match poller_is_initialized {
                            false => {
                                let mut poller = CanisterPoller {
                                    canister_id: gateway_session.canister_id.clone(),
                                    canister_client_session_map: HashMap::new(),
                                    agent: Arc::clone(&agent),
                                };
                                poller.add_session(gateway_session.client_key.clone(), gateway_session.clone());
                                let message_for_client_tx_cl = message_for_client_tx.clone();
                                poller.run_polling(message_for_client_tx_cl).await;
                                println!("Created new poller for canister: {}", poller.canister_id);
                                gateway_server.write().await
                                    .connected_canisters.insert(poller.canister_id, poller);
                            },
                            true => {
                                println!("Added new client session to poller of canister: {}", gateway_session.canister_id);
                                gateway_server.write().await
                                    .connected_canisters.get_mut(&gateway_session.canister_id)
                                    .expect("poller should have already been initialized")
                                    .add_session(gateway_session.client_key.clone(), gateway_session);
                            }
                        }
                    },
                    Err(e) => {
                        println!("{:?}", e);
                    }
                }
            }
        }
    }
}
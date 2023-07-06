use futures_util::{StreamExt, SinkExt};
use std::net::SocketAddr;
use tokio::{net::{TcpListener, TcpStream}, sync::{RwLock, mpsc::{self, UnboundedSender}}, select};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error, Result, Message}, WebSocketStream,
};
use candid::CandidType;
use ed25519_compact::Signature;
use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, to_vec};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

mod canister_methods;

// url for local testing
// for local testing also the agent needs to fetch the root key
const URL: &str = "http://127.0.0.1:4943";
const FETCH_KEY: bool = true;

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
struct MessageFromClient {
    #[serde(with = "serde_bytes")]
    content: Vec<u8>,
    #[serde(with = "serde_bytes")]
    sig: Vec<u8>,
}

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
#[candid_path("ic_cdk::export::candid")]
struct ClientCanisterId {
    client_id: u64,
    canister_id: String,
}

#[derive(Debug)]
struct GatewaySession {
    session_id: u64,
    canister_id: Principal,
}

#[derive(Debug)]
enum IcWsInitializationError {
    CustomError(String),    // error due to the client not following the IC WS initialization protocol
    WsError(Error),     // WebSocket error
}

async fn handle_connection(agent: &Agent, client_addr: SocketAddr, stream: TcpStream, session_id: u64, session_init_tx: UnboundedSender<Result<(GatewaySession, WebSocketStream<TcpStream>), IcWsInitializationError>>) {
    let connection_result = match accept_async(stream).await {
        Ok(mut ws_stream) => {
            if let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Binary(bytes)) => {
                        // TODO: handle possible errors in order not to panic
                        if let Ok(m) = from_slice::<MessageFromClient>(&bytes) {
                            if let Ok(content) = from_slice::<ClientCanisterId>(&m.content) {
                                if let Ok(canister_id) = Principal::from_text(&content.canister_id) {
                                    let client_key =
                                        canister_methods::ws_get_client_key(agent, &canister_id, content.client_id)
                                            .await;
                                    if let Ok(sig) = Signature::from_slice(&m.sig) {
                                        let valid = client_key.verify(&m.content, &sig);
                                        match valid {
                                            Ok(_) => {
                                                let _ret =
                                                    canister_methods::ws_open(agent, &canister_id, m.content, m.sig)
                                                    .await;
                                                println!("New WebSocket connection: {} with session id {}", client_addr, session_id);
                    
                                                Ok((
                                                    GatewaySession {
                                                        session_id,
                                                        canister_id,
                                                    },
                                                    ws_stream
                                                )
                                            )
                                            },
                                            Err(_) => Err(IcWsInitializationError::CustomError(String::from("Client's signature does not verify"))),
                                        }
                                    }
                                    else {
                                        Err(IcWsInitializationError::CustomError(String::from("first message does not contain a valid signature")))
                                    }
                                }
                                else {
                                    Err(IcWsInitializationError::CustomError(String::from("content of first message does not contain a valid principal in canister_id")))
                                }
                            }
                            else {
                                Err(IcWsInitializationError::CustomError(String::from("content of first message is not of type ClientCanisterId")))
                            }
                        }
                        else {
                            Err(IcWsInitializationError::CustomError(String::from("first message is not of type MessageFromClient")))
                        }
                    },
                    Ok(_) => Err(IcWsInitializationError::CustomError(String::from("first message from client should be binary encoded"))),
                    Err(e) => Err(IcWsInitializationError::WsError(e)),
                }
            }
            else {
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
    canister_client_session_map: HashMap<u64, GatewaySession>,
    agent: Arc<Agent>
}

impl CanisterPoller {
    async fn run_polling(&self, message_for_client_tx: UnboundedSender<(u64, CertMessage)>) {
        let agent = Arc::clone(&self.agent);
        let canister_id = self.canister_id;
        tokio::spawn({
            let interval = Duration::from_millis(200);
            let mut nonce: u64 = 0;
            async move {
                loop {
                    let msgs = canister_methods::ws_get_messages(&*agent, &canister_id, nonce).await;

                    for encoded_message in msgs.messages {
                        let client_id = encoded_message.client_id;

                        println!(
                            "Message to client #{} with key {}.",
                            client_id, encoded_message.key
                        );

                        let m = CertMessage {
                            key: encoded_message.key.clone(),
                            val: encoded_message.val,
                            cert: msgs.cert.clone(),
                            tree: msgs.tree.clone(),
                        };

                        if let Err(e) = message_for_client_tx.send((client_id, m)) {
                            println!("Error sendind message to main thread: {}", e);
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

    fn add_session(&mut self, canister_client_id: u64, session: GatewaySession) {
        self.canister_client_session_map.insert(canister_client_id, session);
    }
}

struct GatewayServer {
    next_session_id: u64,
    connected_canisters: HashMap<Principal, CanisterPoller>,
    session_streamers: HashMap<u64, WebSocketStream<TcpStream>>,
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
        next_session_id: 0,
        connected_canisters: HashMap::default(),
        session_streamers: HashMap::default(),
    }));

    let (session_init_tx, mut session_init_rx) = mpsc::unbounded_channel();
    let (message_for_client_tx, mut message_for_client_rx) = mpsc::unbounded_channel();
    let gateway_server_cl = Arc::clone(&gateway_server);
    let agent_cl = Arc::clone(&agent);
    // spawn a task which keeps accepting and handling incoming connection requests from WebSocket clients
    tokio::spawn(async move {
        while let Ok((stream, client_addr)) = listener.accept().await {
            let gateway_server_cl = Arc::clone(&gateway_server_cl);
            let agent_cl = Arc::clone(&agent_cl);
            let session_init_tx_cl = session_init_tx.clone();
            // spawn a connection handler task for each incoming connection 
            tokio::spawn(async move {
                println!("\nClient address: {}", client_addr);
                // ensure that the lock is released before calling handle_connection
                let session_id = {
                    let session_id = gateway_server_cl.read().await.next_session_id;
                    // next_session_id must be updated before handle_connection
                    // otherwise two clients connecting at the same time might end up with the same client_id 
                    gateway_server_cl.write().await.next_session_id += 1;
                    session_id
                };
                handle_connection(&*agent_cl, client_addr, stream, session_id, session_init_tx_cl).await
            });
        }
    });

    loop {
        select! {
            // prioritize relaying updates to alrwady connected clients rather than handling new client connections
            Some((client_id, message)) = message_for_client_rx.recv() => {
                match gateway_server.write().await.session_streamers.get_mut(&client_id) {
                    Some(ws_stream) => {
                        if let Err(e) = ws_stream.send(Message::Binary(to_vec(&message).unwrap())).await {
                            println!("WebSocket error: {}", e);
                        }
                    },
                    None => println!("No client with id: {}", client_id)
                }
            }
            Some(task_result) = session_init_rx.recv() => {
                match task_result {
                    Ok((gateway_session, ws_stream)) => {
                        // ensure that the lock is released before executing the match arms
                        // not doing so would result in a deadlock as the lock is awaited within each arm
                        // but it would not be released by the expression being matched until an arm is executed
                        let poller_is_initialized = {
                            gateway_server.write().await.session_streamers.insert(gateway_session.session_id, ws_stream);
                            gateway_server.read().await.connected_canisters.contains_key(&gateway_session.canister_id)
                        };
                        match poller_is_initialized {
                            false => {
                                let mut poller = CanisterPoller {
                                    canister_id: gateway_session.canister_id.clone(),
                                    canister_client_session_map: HashMap::new(),
                                    agent: Arc::clone(&agent),
                                };
                                poller.add_session(gateway_session.session_id, gateway_session);
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
                                    .expect("poller already initialized")
                                    .add_session(gateway_session.session_id, gateway_session);
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
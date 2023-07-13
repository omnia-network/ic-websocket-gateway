use canister_methods::CertMessages;
use futures_util::{StreamExt, SinkExt, stream::SplitSink, TryStreamExt};
use std::{net::SocketAddr, rc::Rc, cell::RefCell};
use tokio::{net::{TcpListener, TcpStream}, sync::{mpsc::{self, UnboundedSender, UnboundedReceiver}}, select};
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
enum GatewayMessage {
    RelayedFromClient(MessageFromClient),
    FromGateway(Vec<u8>, bool)
}

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
    #[serde(with = "serde_bytes")]
    client_key: Vec<u8>,
    canister_id: String,
}

#[derive(Debug, Clone)]
struct GatewaySession {
    client_key: Vec<u8>,
    canister_id: Principal,
    message_for_client_tx: UnboundedSender<CertMessage>
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
        let canister_id = Principal::from_text(&content.canister_id).map_err(|_| {
            String::from("content of first message does not contain a valid principal in canister_id")
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
        if canister_methods::ws_open(agent, &canister_id, m.content, m.sig).await {
            println!("New WebSocket connection: {}", client_addr);
            Ok((content.client_key, canister_id))
        }
        else {
            Err(String::from("canister could not verify client's signature against public key"))
        }
    }
    else {
        Err(String::from("first message from client should be binary encoded"))
    }
}

async fn handle_connection(agent: &Agent, client_addr: SocketAddr, stream: TcpStream, connection_handler_tx: UnboundedSender<Result<GatewaySession, IcWsInitializationError>>) {
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
                                    // Client has sent a close message, indicating disconnection
                                    println!("WebSocket stream has been closed by the client");
                                    break;
                                }
                                if is_first_message {
                                    // check if client correctly registered its public key in the backend canister
                                    match check_canister_init(agent, client_addr, message.clone()).await {
                                        Ok((client_key, canister_id)) => {
                                            ws_write.send(Message::Text("1".to_string())).await;   // tell the client that the IC WS connection is setup correctly
        
                                            let message_for_client_tx_cl = message_for_client_tx.clone();
                                            connection_handler_tx.send(
                                                Ok(
                                                    GatewaySession {
                                                        client_key,
                                                        canister_id,
                                                        message_for_client_tx: message_for_client_tx_cl,
                                                    },
                                                )
                                            ).expect("channel should be open");
                                        },
                                        Err(e) => {
                                            ws_write.send(Message::Text("0".to_string())).await;   // tell the client that the setup of the IC WS connection failed
                                            // Err(IcWsInitializationError::CustomError(e))
                                        }
                                    };
                                    is_first_message = false;
                                }
                                else {
                                    println!("Client sent message: {:?}", message);
                                }
                                // Process the message and send a response if needed
                                // ...
                            }
                            Ok(None) => {
                                // ws_write.send(Message::Text("0".to_string())).await;   // tell the client that the setup of the IC WS connection failed
                                // Err(IcWsInitializationError::CustomError(String::from("client should send first message")));
                                break;
                            }
                            Err(err) => {
                                println!("Error reading from WebSocket stream: {}", err);
                                // Err(IcWsInitializationError::WsError(err));
                                break;
                            }
                        }
                    }
                    Some(message) = message_for_client_rx.recv() => {
                        ws_write.send(Message::Binary(to_vec(&message).unwrap())).await;
                    }
                }
            }
        }
        Err(e) => {
            // Err(IcWsInitializationError::WsError(e));
        }
    };
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
    async fn run_polling(&self, clients_updates_tx: UnboundedSender<CertMessages>) {
        let agent = Arc::clone(&self.agent);
        let canister_id = self.canister_id;
        tokio::spawn({
            let interval = Duration::from_millis(200);
            let mut nonce: u64 = 0;
            async move {
                loop {
                    let msgs = canister_methods::ws_get_messages(&*agent, &canister_id, nonce).await;
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
                    clients_updates_tx.send(msgs);

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
    client_session_map: HashMap<Vec<u8>, GatewaySession>,
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
    };

    let (connection_handler_tx, mut connection_handler_rx) = mpsc::unbounded_channel();
    let (clients_updates_tx, mut clients_updates_rx): (UnboundedSender<CertMessages>, UnboundedReceiver<CertMessages>) = mpsc::unbounded_channel();
    let agent_cl = Arc::clone(&agent);
    // spawn a task which keeps accepting and handling incoming connection requests from WebSocket clients
    tokio::spawn(async move {
        while let Ok((stream, client_addr)) = listener.accept().await {
            let agent_cl = Arc::clone(&agent_cl);
            let connection_handler_tx_cl = connection_handler_tx.clone();
            // spawn a connection handler task for each incoming connection 
            tokio::spawn(async move {
                println!("\nClient address: {}", client_addr);
                handle_connection(&*agent_cl, client_addr, stream, connection_handler_tx_cl).await
            });
        }
    });

    loop {
        select! {
            Some(msgs) = clients_updates_rx.recv() => {
                // println!("Triggered message handler");
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

                    if let Err(e) = gateway_server.client_session_map.get(&client_key).expect("should have channel with client's thread").message_for_client_tx.send(m) {
                        println!("Error sending canister message to client's thread: {}", e);
                    }
                }
            }
            Some(connection_result) = connection_handler_rx.recv() => {
                // println!("Triggered connection handler");
                match connection_result {
                    Ok(gateway_session) => {
                        // notify canister that it can now send messages for the client corresponding to client_key
                        let gateway_message = GatewayMessage::FromGateway(gateway_session.client_key.clone(), true);
                        canister_methods::ws_message(&*agent, &gateway_session.canister_id, to_vec(&gateway_message).unwrap()).await;

                        gateway_server.client_session_map.insert(gateway_session.client_key.clone(), gateway_session.clone());
                        match gateway_server.connected_canisters.contains_key(&gateway_session.canister_id) {
                            false => {
                                let mut poller = CanisterPoller {
                                    canister_id: gateway_session.canister_id.clone(),
                                    canister_client_session_map: HashMap::new(),
                                    agent: Arc::clone(&agent),
                                };
                                poller.add_session(gateway_session.client_key.clone(), gateway_session.clone());
                                let clients_updates_tx_cl = clients_updates_tx.clone();
                                poller.run_polling(clients_updates_tx_cl).await;
                                println!("Created new poller for canister: {}", poller.canister_id);
                                gateway_server
                                    .connected_canisters.insert(poller.canister_id, poller);
                            },
                            true => {
                                println!("Added new client session to poller of canister: {}", gateway_session.canister_id);
                                gateway_server
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
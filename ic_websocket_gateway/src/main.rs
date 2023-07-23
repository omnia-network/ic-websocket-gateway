use candid::CandidType;
use canister_methods::{
    CanisterFirstMessageContent, CanisterOutputCertifiedMessages, ClientPublicKey,
    RelayedClientMessage,
};
use ed25519_compact::{PublicKey, Signature};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use ic_cdk::println;
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, to_vec};
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

use crate::canister_methods::CanisterIncomingMessage;

mod canister_methods;

// url for local testing
// for local testing also the agent needs to fetch the root key
const URL: &str = "http://127.0.0.1:4943";
const FETCH_KEY: bool = true;

#[derive(Debug, Clone)]
struct GatewaySession {
    client_id: u64,
    client_key: ClientPublicKey,
    canister_id: Principal,
    message_for_client_tx: UnboundedSender<CertifiedMessage>,
}

#[derive(Debug)]
enum IcWsError {
    InitializationError(String), // error due to the client not following the IC WS initialization protocol
    WsError(Error),              // WebSocket error
    WsClose(String),             // WebSocket closed by client
}

async fn check_canister_init(
    agent: &Agent,
    message: Message,
) -> Result<(ClientPublicKey, Principal), String> {
    if let Message::Binary(bytes) = message {
        let m = from_slice::<RelayedClientMessage>(&bytes)
            .map_err(|_| String::from("first message is not of type MessageFromClient"))?;
        let content = from_slice::<CanisterFirstMessageContent>(&m.content).map_err(|_| {
            String::from("content of first message is not of type ClientCanisterId")
        })?;
        let sig = Signature::from_slice(&m.sig)
            .map_err(|_| String::from("first message does not contain a valid signature"))?;
        let public_key = PublicKey::from_slice(&content.client_key)
            .map_err(|_| String::from("first message does not contain a valid public key"))?;
        public_key
            .verify(&m.content, &sig)
            .map_err(|_| String::from("client's signature does not verify against public key"))?;
        canister_methods::ws_open(agent, &content.canister_id, m.content, m.sig).await
    } else {
        Err(String::from(
            "first message from client should be binary encoded",
        ))
    }
}

async fn handle_connection(
    client_id: u64,
    agent: &Agent,
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
                    // wait for incoming message from client
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
                                    match check_canister_init(agent, message.clone()).await {
                                        Ok((client_key, canister_id)) => {
                                            // tell the client that the IC WS connection is setup correctly
                                            ws_write.send(Message::Text("1".to_string())).await.map_err(|e| {
                                                IcWsError::WsError(e)
                                            })?;

                                            // create a new GatewaySession and send it to the main thread
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
                    // wait for message to send to client
                    Some(message) = message_for_client_rx.recv() => {
                        // send canister message to client, cbor encoded
                        ws_write.send(Message::Binary(to_vec(&message).unwrap())).await.map_err(|e| {
                            IcWsError::WsError(e)
                        })?;
                    }
                }
            }
        },
        Err(e) => return Err(IcWsError::WsError(e)),
    }
}

#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct CertifiedMessage {
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
    async fn run_polling(
        &self,
        mut new_client_channel_rx: UnboundedReceiver<ClientPollerChannelData>,
    ) {
        // channels used to communicate with client's WebSocket task
        let mut client_channels: HashMap<ClientPublicKey, UnboundedSender<CertifiedMessage>> =
            HashMap::new();
        let mut nonce: u64 = 0;
        loop {
            select! {
                // receive channel used to send canister updates to new client's task
                Some(channel_data) = new_client_channel_rx.recv() => {
                    match channel_data {
                        ClientPollerChannelData::NewClientChannel(client_key, client_channel) => {
                            println!("Adding new client poller channel: canister: {}, client {:?}", self.canister_id, client_key);
                            client_channels.insert(client_key, client_channel);
                        },
                        ClientPollerChannelData::ClientDisconnected(client_key) => {
                            println!("Removing client poller channel: canister: {}, client {:?}", self.canister_id, client_key);
                            client_channels.remove(&client_key);
                            // exit task if last client disconnected
                            if client_channels.is_empty() {
                                println!("Last client disconnected, terminating poller task: canister {}", self.canister_id);
                                break;
                            }
                        }
                    }
                }
                // poll canister for updates
                msgs = get_canister_updates(&self.agent, self.canister_id, nonce) => {
                    for encoded_message in msgs.messages {
                        let client_key = encoded_message.client_key;

                        println!(
                            "Message to client: client key: {:?}, message key {}.",
                            client_key, encoded_message.key
                        );

                        let m = CertifiedMessage {
                            key: encoded_message.key.clone(),
                            val: encoded_message.val,
                            cert: msgs.cert.clone(),
                            tree: msgs.tree.clone(),
                        };

                        match client_channels.get(&client_key) {
                            Some(client_channel_rx) => {
                                if let Err(e) = client_channel_rx.send(m) {
                                    println!("Client's thread terminated: {}", e);
                                }
                            },
                            None => println!("Connection with client closed before message could be delivered")
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
                }

            }
        }
    }
}

async fn get_canister_updates(
    agent: &Agent,
    canister_id: Principal,
    nonce: u64,
) -> CanisterOutputCertifiedMessages {
    tokio::time::sleep(Duration::from_millis(200)).await;
    canister_methods::ws_get_messages(agent, &canister_id, nonce)
        .await
        .unwrap()
}

async fn remove_client_from_server(
    gateway_server: &mut GatewayServer,
    client_id: u64,
    agent: &Agent,
) {
    match gateway_server.client_key_map.remove(&client_id) {
        Some(client_key) => {
            let gateway_session = gateway_server
                .client_session_map
                .remove(&client_key.clone())
                .expect("gateway session should be registered");
            // close client connection on canister
            if let Err(e) = canister_methods::ws_close(
                &*agent,
                &gateway_session.canister_id,
                client_key.clone(),
            )
            .await
            {
                println!("Calling ws_close on canister failed: {}", e);
            }

            // remove client's channel from poller, if it exists
            match gateway_server
                .connected_canisters
                .get_mut(&gateway_session.canister_id.clone())
            {
                Some(canister_channel) => {
                    canister_channel
                        .send(ClientPollerChannelData::ClientDisconnected(
                            client_key.clone(),
                        ))
                        .unwrap();
                },
                None => (),
            }
        },
        None => {
            println!("Client closed connection before being registered");
        },
    }
}

#[derive(Debug, Clone)]
enum ClientPollerChannelData {
    NewClientChannel(ClientPublicKey, UnboundedSender<CertifiedMessage>),
    ClientDisconnected(ClientPublicKey),
}

struct GatewayServer {
    connected_canisters: HashMap<Principal, UnboundedSender<ClientPollerChannelData>>,
    client_session_map: HashMap<ClientPublicKey, GatewaySession>,
    client_key_map: HashMap<u64, ClientPublicKey>,
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

    let mut gateway_server = GatewayServer {
        connected_canisters: HashMap::default(),
        client_session_map: HashMap::default(),
        client_key_map: HashMap::default(),
    };

    let (connection_handler_tx, mut connection_handler_rx) = mpsc::unbounded_channel();
    let agent_cl = Arc::clone(&agent);
    let client_id = Arc::new(Mutex::new(0)); // needed to know which gateway_session to delete in case of error or WS closed

    // spawn a task which keeps accepting and handling incoming connection requests from WebSocket clients
    tokio::spawn(async move {
        while let Ok((stream, _client_addr)) = listener.accept().await {
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
                let end_connection_result =
                    handle_connection(next_client_id, &*agent_cl, stream, connection_handler_tx_cl)
                        .await;
                println!("Client connection terminated: {:?}", end_connection_result);
            });
        }
    });

    loop {
        select! {
            // wait for new connection handler task to send the client'c connection result
            // which is either a GatewaySession if the connection was successful
            // or the client_id if the connection was closed before the client was registered
            Some(connection_result) = connection_handler_rx.recv() => {
                match connection_result {
                    Ok(gateway_session) => {
                        gateway_server.client_key_map.insert(gateway_session.client_id.clone(), gateway_session.client_key.clone());
                        gateway_server.client_session_map.insert(gateway_session.client_key.clone(), gateway_session.clone());
                        // check if client is connecting to a canister that is not yet being polled
                        // if so, create new poller task

                        let client_poller_channel_data = ClientPollerChannelData::NewClientChannel(gateway_session.client_key.clone(), gateway_session.message_for_client_tx.clone());

                        let client_channel_tx = gateway_server.connected_canisters.get_mut(&gateway_session.canister_id.clone());

                        let needs_new_poller = match client_channel_tx {
                            Some(client_channel_tx) => {
                                if client_channel_tx.send(client_poller_channel_data.clone()).is_err() {
                                    // poller task has terminated, remove it from the map
                                    gateway_server.connected_canisters.remove(&gateway_session.canister_id.clone());
                                    true
                                } else {
                                    false
                                }
                            },
                            None => true
                        };

                        if needs_new_poller {
                            // channel used to communicate with the poller task
                            // sender side of new client's channel are sent to the poller using new_client_channel_tx
                            // so that it can use them to send canister updates directly to the client's task
                            let (new_client_channel_tx, new_client_channel_rx) = mpsc::unbounded_channel();

                            // register new poller and the channel used to send client's channels to it
                            gateway_server.connected_canisters.insert(gateway_session.canister_id.clone(), new_client_channel_tx.clone());
                            let agent = Arc::clone(&agent);

                            // spawn new canister poller task
                            tokio::spawn({
                                async move {
                                    let poller = CanisterPoller {
                                        canister_id: gateway_session.canister_id.clone(),
                                        agent,
                                    };
                                    println!("Created new poller: canister: {}", poller.canister_id);
                                    poller.run_polling(new_client_channel_rx).await;
                                    println!("Poller task terminated: canister {}", poller.canister_id);
                                }
                            });

                            new_client_channel_tx.send(client_poller_channel_data).unwrap();
                        }

                        // notify canister that it can now send messages for the client corresponding to client_key
                        let gateway_message = CanisterIncomingMessage::IcWebSocketEstablished(gateway_session.client_key.clone());
                        if let Err(e) = canister_methods::ws_message(&*agent, &gateway_session.canister_id, gateway_message).await {
                            println!("Calling ws_message on canister failed: {}", e);

                            remove_client_from_server(&mut gateway_server, gateway_session.client_id, &agent).await
                        }
                    },
                    Err(client_id) => {
                        remove_client_from_server(&mut gateway_server, client_id, &agent).await
                    }
                }

                println!("{} clients registered", gateway_server.client_session_map.len());
            }
        }
    }
}

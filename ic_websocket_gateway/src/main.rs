use candid::CandidType;
use canister_methods::{CanisterOutputCertifiedMessages, ClientPublicKey};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use ic_agent::{export::Principal, identity::BasicIdentity, Agent};
use serde::{Deserialize, Serialize};
use serde_cbor::to_vec;
use std::{collections::HashMap, fs, path::Path, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error, Message, Result},
};

use crate::canister_methods::{CanisterIncomingMessage, CanisterWsOpenResultValue};

mod canister_methods;

// url for local testing
// for local testing also the agent needs to fetch the root key
const URL: &str = "http://127.0.0.1:4943";
const FETCH_KEY: bool = true;

/// possible states of the WebSocket connection:
/// - established
/// - closed
#[derive(Debug, Clone)]
enum WsConnectionState {
    /// WebSocket connection between client and WS Gateway established
    // does not imply that the IC WebSocket connection has also been established
    ConnectionEstablished(GatewaySession),
    /// WebSocket connection between client and WS Gateway closed
    ConnectionClosed(u64),
}

/// contains the information needed by the WS Gateway to maintain the state of the WebSocket connection
#[derive(Debug, Clone)]
struct GatewaySession {
    client_id: u64,
    client_key: ClientPublicKey,
    canister_id: Principal,
    message_for_client_tx: UnboundedSender<CertifiedMessage>,
    nonce: u64,
}

/// possible errors that can occur during a IC WebSocket connection
#[derive(Debug)]
enum IcWsError {
    /// error due to the client not following the IC WS initialization protocol
    InitializationError(String),
    /// WebSocket error
    WsError(Error),
    /// WebSocket closed by client
    WsClose(String),
}

async fn handle_client_connection(
    client_id: u64,
    agent: &Agent,
    stream: TcpStream,
    client_connection_handler_tx: UnboundedSender<WsConnectionState>,
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
                                // check if the WebSocket connection is closed
                                if message.is_close() {
                                    // let the main task know that it should remove the client's session from the WS Gateway state
                                    client_connection_handler_tx.send(
                                        WsConnectionState::ConnectionClosed(client_id)
                                    ).expect("channel should be open on the main thread");
                                    // return error so that the connection handler task can terminate
                                    return Err(IcWsError::WsClose(format!("WebSocket stream has been closed by the client with id: {}", client_id)));
                                }
                                // check if it is the first message being sent by the client via WebSocket
                                if is_first_message {
                                    // check if client followed the IC WebSocket connection establishment protocol
                                    match canister_methods::check_canister_init(agent, message.clone()).await {
                                        Ok(CanisterWsOpenResultValue {
                                            client_key,
                                            canister_id,
                                            // nonce is used by a new poller to know which message nonce to start polling from (if needed)
                                            // the nonce is obtained from the canister every time a client connects and the ws_open is called by the WS Gateway
                                            nonce,
                                        }) => {
                                            // let the client know that the IC WS connection is setup correctly
                                            ws_write.send(Message::Text("1".to_string())).await.map_err(|e| {
                                                IcWsError::WsError(e)
                                            })?;

                                            // create a new sender side of the channel which will be used to send canister messages
                                            // from the poller task directly to the client's connection handler task
                                            let message_for_client_tx_cl = message_for_client_tx.clone();
                                            // instantiate a new GatewaySession and send it to the main thread
                                            client_connection_handler_tx.send(
                                                WsConnectionState::ConnectionEstablished(
                                                    GatewaySession {
                                                        client_id,
                                                        client_key,
                                                        canister_id,
                                                        message_for_client_tx: message_for_client_tx_cl,
                                                        nonce,
                                                    },
                                                )
                                            ).expect("channel should be open on the main thread");
                                        },
                                        Err(e) => {
                                            // tell the client that the setup of the IC WS connection failed
                                            ws_write.send(Message::Text("0".to_string())).await.map_err(|e| {
                                                IcWsError::WsError(e)
                                            })?;
                                            // if this branch is executed, the Ok branch is never been executed, hence the WS Gateway state
                                            // does not contain any session for this client and therefore there is no cleanup needed
                                            return Err(IcWsError::InitializationError(e));
                                        }
                                    };
                                    // makes sure that this branch is executed at most once
                                    is_first_message = false;
                                }
                                else {
                                    // TODO: handle incoming message from client
                                    // println!("Client sent message: {:?}", message);
                                }
                            }
                            // in these cases, client's session should have been cleaned up on the WS Gateway state already
                            // once the connection handler received Message::Close
                            // therefore, no additional cleanup is needed
                            Ok(None) => return Err(IcWsError::WsError(Error::AlreadyClosed)),
                            Err(err) => return Err(IcWsError::WsError(err))
                        }
                    }
                    // wait for canister message to send to client
                    Some(message) = message_for_client_rx.recv() => {
                        // relay canister message to client, cbor encoded
                        ws_write.send(Message::Binary(to_vec(&message).unwrap())).await.map_err(|e| {
                            IcWsError::WsError(e)
                        })?;
                    }
                }
            }
        },
        // no cleanup needed on the WS Gateway has the client's session has never been created
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
        mut poller_channel_rx: UnboundedReceiver<ClientPollerChannelData>,
        mut nonce: u64,
    ) {
        // channels used to communicate with client's WebSocket task
        let mut client_channels: HashMap<ClientPublicKey, UnboundedSender<CertifiedMessage>> =
            HashMap::new();
        println!(
            "Started poller: canister: {}, nonce: {}",
            self.canister_id, nonce
        );
        loop {
            select! {
                // receive channel used to send canister updates to new client's task
                Some(channel_data) = poller_channel_rx.recv() => {
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

fn add_client_to_server(gateway_server: &mut GatewayServer, gateway_session: GatewaySession) {
    let client_key = gateway_session.client_key.clone();
    let client_id = gateway_session.client_id.clone();

    gateway_server
        .client_key_map
        .insert(client_id, client_key.clone());
    gateway_server
        .client_session_map
        .insert(client_key, gateway_session);
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

fn load_key_pair() -> ring::signature::Ed25519KeyPair {
    if !Path::new("./data").is_dir() {
        fs::create_dir("./data").unwrap();
    }

    if !Path::new("./data/key_pair").is_file() {
        let rng = ring::rand::SystemRandom::new();
        let key_pair = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
            .expect("Could not generate a key pair.");
        // TODO: print out seed phrase
        fs::write("./data/key_pair", key_pair.as_ref()).unwrap();
        ring::signature::Ed25519KeyPair::from_pkcs8(key_pair.as_ref())
            .expect("Could not read the key pair.")
    } else {
        let key_pair = fs::read("./data/key_pair").unwrap();
        ring::signature::Ed25519KeyPair::from_pkcs8(&key_pair)
            .expect("Could not read the key pair.")
    }
}

#[derive(Debug, Clone)]
enum ClientPollerChannelData {
    NewClientChannel(ClientPublicKey, UnboundedSender<CertifiedMessage>),
    ClientDisconnected(ClientPublicKey),
}

/// state of the WS Gateway containing:
/// - canisters it is polling
/// - sessions with the clients connected to it via WebSocket
/// - id of each client
struct GatewayServer {
    /// maps the principal of the canister to the sender side of the channel used to communicate with the poller task
    connected_canisters: HashMap<Principal, UnboundedSender<ClientPollerChannelData>>,
    /// maps the client's public key to the state of the client's session
    client_session_map: HashMap<ClientPublicKey, GatewaySession>,
    /// maps the client id to its public key
    // needed because when a client disconnects, we only know its id but in order to clean the state of the client's session
    // we need to know the public key of the client
    client_key_map: HashMap<u64, ClientPublicKey>,
}

#[tokio::main]
async fn main() {
    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(&addr).await.expect("Can't listen");
    println!("Listening on: {}", addr);

    let key_pair = load_key_pair();
    let identity = BasicIdentity::from_key_pair(key_pair);

    let agent = Arc::new(canister_methods::get_new_agent(URL, identity, FETCH_KEY).await);
    let agent_cl = Arc::clone(&agent);

    println!(
        "Gateway Agent principal: {}",
        agent.get_principal().expect("Principal should be set")
    );

    let mut gateway_server = GatewayServer {
        connected_canisters: HashMap::default(),
        client_session_map: HashMap::default(),
        client_key_map: HashMap::default(),
    };

    // [main task]                         [client connection handler task]
    // client_connection_handler_rx -----> client_connection_handler_tx

    // channel used to send the state of the client connection
    // the client connection handler task sends the session information when the WebSocket connection is established and
    // the id the of the client when the connection is closed
    let (client_connection_handler_tx, mut client_connection_handler_rx) =
        mpsc::unbounded_channel();

    // spawn a task which keeps accepting incoming connection requests from WebSocket clients
    tokio::spawn(async move {
        let mut next_client_id = 0; // needed to know which gateway_session to delete in case of error or WS closed
        while let Ok((stream, _client_addr)) = listener.accept().await {
            let agent_cl = Arc::clone(&agent_cl);
            let client_connection_handler_tx_cl = client_connection_handler_tx.clone();
            // spawn a connection handler task for each incoming connection
            let current_client_id = next_client_id;
            tokio::spawn(async move {
                println!("\nNew client id: {}", current_client_id);
                let end_connection_result = handle_client_connection(
                    current_client_id,
                    &*agent_cl,
                    stream,
                    client_connection_handler_tx_cl,
                )
                .await;
                println!("Client connection terminated: {:?}", end_connection_result);
            });
            next_client_id += 1;
        }
    });

    loop {
        select! {
            // wait for new connection handler task to send the client'c connection result
            // which is either a GatewaySession if the connection was successful
            // or the client_id if the connection was closed before the client was registered
            Some(connection_state) = client_connection_handler_rx.recv() => {
                match connection_state {
                    WsConnectionState::ConnectionEstablished(gateway_session) => {
                        // add client's session state to the WS Gateway state
                        add_client_to_server(&mut gateway_server, gateway_session.clone());

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
                            // [main task]              [poller task]
                            // poller_channel_tx -----> poller_channel_rx

                            // channel used to communicate with the poller task
                            // the channel is used to send to the poller the sender side of a new client's channel
                            // so that the poller can send canister messages directly to the client's task
                            let (poller_channel_tx, poller_channel_rx) = mpsc::unbounded_channel();

                            // register new poller and the channel used to send client's channels to it
                            gateway_server.connected_canisters.insert(gateway_session.canister_id.clone(), poller_channel_tx.clone());
                            let agent = Arc::clone(&agent);

                            // spawn new canister poller task
                            tokio::spawn({
                                async move {
                                    let poller = CanisterPoller {
                                        canister_id: gateway_session.canister_id.clone(),
                                        agent,
                                    };
                                    println!("Created new poller: canister: {}", poller.canister_id);
                                    // if a new poller thread is started due to a client connection, the poller needs to know the nonce of the last polled message
                                    // as an old poller thread (closed due to all clients disconnecting) might have already polled messages from the canister
                                    // the new poller thread should not get those same messages again
                                    poller.run_polling(poller_channel_rx, gateway_session.nonce).await;
                                    println!("Poller task terminated: canister {}", poller.canister_id);
                                }
                            });

                            poller_channel_tx.send(client_poller_channel_data).unwrap();
                        }

                        // notify canister that it can now send messages for the client corresponding to client_key
                        let gateway_message = CanisterIncomingMessage::IcWebSocketEstablished(gateway_session.client_key);
                        if let Err(e) = canister_methods::ws_message(&*agent, &gateway_session.canister_id, gateway_message).await {
                            println!("Calling ws_message on canister failed: {}", e);

                            remove_client_from_server(&mut gateway_server, gateway_session.client_id, &agent).await
                        }
                    },
                    WsConnectionState::ConnectionClosed(client_id) => {
                        // cleanup client's session from WS Gateway state
                        remove_client_from_server(&mut gateway_server, client_id, &agent).await
                    }
                }

                println!("{} clients registered", gateway_server.client_session_map.len());
            }
        }
    }
}

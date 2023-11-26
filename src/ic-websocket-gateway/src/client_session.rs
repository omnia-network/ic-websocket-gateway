use crate::{
    canister_methods::{CanisterToClientMessage, CanisterWsOpenArguments, ClientKey},
    canister_poller::IcWsCanisterUpdate,
    manager::{ClientSender, GatewayState, PollerState},
};
use candid::{decode_args, Principal};
use dashmap::{mapref::entry::Entry, DashMap};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use ic_agent::{
    agent::{Envelope, EnvelopeContent},
    Agent, AgentError,
};
use serde::{Deserialize, Serialize};
use serde_cbor::{from_slice, to_vec};
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    select,
    sync::mpsc::{Receiver, Sender},
};
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};
use tracing::{debug, error, info, span, trace, warn, Instrument, Level, Span};

#[derive(Serialize, Deserialize)]
struct GatewayHandshakeMessage {
    gateway_principal: Principal,
}

/// Message sent by the client using the custom @dfinity/agent (via WS)
#[derive(Serialize, Deserialize)]
struct ClientRequest<'a> {
    /// Envelope of the signed request to the IC
    envelope: Envelope<'a>,
}

/// possible states of an IC WebSocket session
#[derive(Debug, Clone)]
pub enum IcWsSessionState {
    Init,
    Setup(Option<PollerState>, Principal),
    Open,
    Closed((ClientKey, Principal)),
}

impl Eq for IcWsSessionState {}

impl PartialEq for IcWsSessionState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (IcWsSessionState::Init, IcWsSessionState::Init) => true,
            (
                IcWsSessionState::Setup(_, canister_id1),
                IcWsSessionState::Setup(_, canister_id2),
            ) => canister_id1 == canister_id2,
            (IcWsSessionState::Open, IcWsSessionState::Open) => true,
            (IcWsSessionState::Closed(canister_id1), IcWsSessionState::Closed(canister_id2)) => {
                canister_id1 == canister_id2
            },
            _ => false,
        }
    }
}

/// possible errors that can occur during an IC WebSocket session
#[derive(Debug, Clone)]
pub enum IcWsError {
    /// error due to the client not following the IC WS protocol
    IcWsProtocol(String),
    /// WebSocket error
    WebSocket(String),
}

/// contains the information needed by the WS Gateway to maintain the state of the WebSocket session
pub struct ClientSession<S: AsyncRead + AsyncWrite + Unpin> {
    client_id: u64,
    client_key: Option<ClientKey>,
    canister_id: Option<Principal>,
    message_for_client_tx: Option<Sender<IcWsCanisterUpdate>>,
    message_for_client_rx: Receiver<IcWsCanisterUpdate>,
    ws_write: SplitSink<WebSocketStream<S>, Message>,
    ws_read: SplitStream<WebSocketStream<S>>,
    session_state: IcWsSessionState,
    gateway_state: GatewayState,
    agent: Arc<Agent>,
    client_connection_span: Span,
}

impl<S: AsyncRead + AsyncWrite + Unpin> ClientSession<S> {
    pub async fn init(
        client_id: u64,
        gateway_principal: Principal,
        message_for_client_tx: Sender<IcWsCanisterUpdate>,
        message_for_client_rx: Receiver<IcWsCanisterUpdate>,
        ws_write: SplitSink<WebSocketStream<S>, Message>,
        ws_read: SplitStream<WebSocketStream<S>>,
        gateway_state: GatewayState,
        agent: Arc<Agent>,
        client_connection_span: Span,
    ) -> Result<Self, IcWsError> {
        let mut client_session = Self {
            client_id,
            client_key: None,
            canister_id: None,
            message_for_client_tx: Some(message_for_client_tx),
            message_for_client_rx,
            ws_write,
            ws_read,
            session_state: IcWsSessionState::Init,
            gateway_state,
            agent,
            client_connection_span,
        };

        // as soon as the WS connection with the client is established, send the gateway principal
        // needed because the client doesn't know the principal of the gateway it is connecting to but only it's IP
        // however, the client has to tell the canister CDK which principal is authorized to poll its updates from the canister queue,
        // the returned principal will be included by the client in the first envelope it sends via WS

        let handshake_message = GatewayHandshakeMessage { gateway_principal };

        if let Err(e) = client_session
            .send_ws_message_to_client(Message::Binary(
                serialize(handshake_message).expect("Principal should be serializable"),
            ))
            .await
        {
            return Err(IcWsError::WebSocket(format!(
                "Error sending handshake message to client: {:?}",
                e
            )));
        }

        Ok(client_session)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> ClientSession<S> {
    pub async fn update_state(&mut self) -> Result<Option<IcWsSessionState>, IcWsError> {
        let old_session_state = self.session_state.clone();
        select! {
            client_update = self.ws_read.next() => {
                match client_update {
                    Some(Ok(ws_message)) => {
                        match old_session_state {
                            IcWsSessionState::Init => {
                                if ws_message.is_close() {
                                    trace!("Client closed connection while in Init state");
                                    self.session_state = IcWsSessionState::Closed((
                                        self.client_key.clone().expect("must be set"), self.canister_id.expect("must be set")
                                    ));
                                } else {
                                    // upon receiving a message while the session is Init, check if the message is valid
                                    // if not return an error, otherwise set the session state to Setup
                                    // if multiple messages are received while in Init state, 'check_setup_transition' will
                                    // return an error as the client shall not send more than one message while in Init state
                                    let setup_state = self.check_setup_transition(ws_message).await?;
                                    self.session_state = setup_state;
                                }
                            },
                            IcWsSessionState::Setup(_, _) => {
                                // upon receiving a message while the session is Setup, discard the message
                                // and return an error as the client shall not send a message while in Setup state
                                // this implies a bug in the client SDK
                                error!("Received client message while in Setup state");
                                return Err(IcWsError::IcWsProtocol(String::from(
                                    "Client shall not send messages while in Setup state",
                                )))
                            },
                            IcWsSessionState::Open => {
                                if ws_message.is_close() {
                                    trace!("Client closed session while in Open state");
                                    self.session_state = IcWsSessionState::Closed((
                                        self.client_key.clone().expect("must be set"), self.canister_id.expect("must be set")
                                    ));
                                } else {
                                    // upon receiving a message while the session is Open, immediately relay the client messages to the IC
                                    // this does not result in a state transition
                                    self.relay_call_request_to_ic(ws_message).await?;
                                }
                            }
                            IcWsSessionState::Closed(_) => {
                                // upon receiving a message while the session is Closed, discard the message
                                // and return an error as this shall not be possible
                                // this implies a bug in the WS Gateway
                                error!("Received client message while in Closed state");
                                return Err(IcWsError::IcWsProtocol(String::from(
                                    "Client shall not send messages while in Closed state",
                                )))
                            }
                        }
                    },
                    Some(Err(e)) => {
                        self.session_state = IcWsSessionState::Closed((
                            self.client_key.clone().expect("must be set"), self.canister_id.expect("must be set")
                        ));
                        return Err(IcWsError::WebSocket(format!("Error receiving message from client: {:?}", e)));
                    },
                    None => {
                        self.session_state = IcWsSessionState::Closed((
                            self.client_key.clone().expect("must be set"), self.canister_id.expect("must be set")
                        ));
                        return Err(IcWsError::WebSocket(String::from("Client connection already closed")));
                    },
                }
            },
            canister_update = self.message_for_client_rx.recv() => {
                match canister_update {
                    Some(IcWsCanisterUpdate::Message((canister_message, _parent_span))) => {
                        match old_session_state {
                            IcWsSessionState::Init => {
                                return Err(IcWsError::IcWsProtocol(String::from(
                                    "Canister shall not send messages while in Init state",
                                )))
                            },
                            IcWsSessionState::Setup(_, _) => {
                                let open_state = self.check_open_transition(canister_message).await?;
                                self.session_state = open_state;
                            },
                            IcWsSessionState::Open => {
                                // once the connection is open, immediately relay the canister messages to the client via the WS
                                self.relay_canister_message(canister_message).await?
                            },
                            _ => unimplemented!("TODO")
                        }
                    }
                    _ => unimplemented!("TODO"),
                }
            }
        }
        if self.session_state != old_session_state {
            Ok(Some(self.session_state.clone()))
        } else {
            Ok(None)
        }
    }

    async fn check_setup_transition(
        &mut self,
        ws_message: Message,
    ) -> Result<IcWsSessionState, IcWsError> {
        match self.inspect_ic_ws_open_message(ws_message.clone()).await {
            // if the IC WS connection is setup, create a new client session and send it to the main task
            Ok((client_key, canister_id)) => {
                // replace the field with the canister_id received in the first envelope
                // this shall not be updated anymore
                // if canister_id is already set in the struct, we return an error as inspect_ic_ws_open_message shall only be called once
                if !self.canister_id.replace(canister_id.clone()).is_none()
                    || !self.client_key.replace(client_key.clone()).is_none()
                {
                    // if the canister_id or client_key field was already set,
                    // it means that the client sent the WS open message twice,
                    // which it shall not do
                    // therefore, return an error
                    return Err(IcWsError::IcWsProtocol(String::from(
                        "canister_id or client_key field was set twice",
                    )));
                }
                trace!("Validated WS open message");

                // SAFETY:
                // first, update the gateway state
                // only then, relay the message to the IC
                // this is necessary to guarantee that once the poller retrieves the response to the WS open message,
                // the poller sees (in the gateway state) the sending side of the channel needed to relay the response to the client session handler.
                // the message cannot be relayed before doing so because if a poller is already running,
                // it might poll a response for the connecting client before it gets the sending side of the channel

                // TODO: figure out if this is actually atomic
                let poller_state = match self.gateway_state.entry(canister_id) {
                    Entry::Occupied(mut entry) => {
                        // the poller has already been started
                        // add client key and sender end of the channel to the poller state
                        let poller_state = entry.get_mut();
                        poller_state.insert(
                            client_key,
                            ClientSender {
                                sender: self
                                    .message_for_client_tx
                                    .take()
                                    .expect("must be set once"),
                                span: Span::current(),
                            },
                        );
                        None
                    },
                    Entry::Vacant(entry) => {
                        // the poller has not been started yet
                        // initialize the poller state and add client key and sender end of the channel
                        let poller_state =
                            Arc::new(DashMap::with_capacity_and_shard_amount(1024, 1024));
                        poller_state.insert(
                            client_key,
                            ClientSender {
                                sender: self
                                    .message_for_client_tx
                                    .take()
                                    .expect("must be set once"),
                                span: Span::current(),
                            },
                        );
                        entry.insert(Arc::clone(&poller_state));
                        Some(Arc::clone(&poller_state))
                    },
                };

                // TODO: figure out if it is guaranteed that all threads see the state of the updated state of the gateway
                //       before relaying the message to the IC
                self.relay_call_request_to_ic(ws_message).await?;

                // client session is now Setup
                Ok(IcWsSessionState::Setup(poller_state, canister_id))
            },
            // in case of other errors, we report them and terminate the connection handler task
            Err(e) => {
                return Err(IcWsError::IcWsProtocol(format!(
                    "IC WS setup failed. Error: {:?}",
                    e
                )));
            },
        }
    }

    async fn check_open_transition(
        &mut self,
        canister_message: CanisterToClientMessage,
    ) -> Result<IcWsSessionState, IcWsError> {
        // if relaying the first canister message to the client succeeds, the client session is Open
        self.relay_canister_message(canister_message).await?;
        Ok(IcWsSessionState::Open)
    }

    /// relays the client's request to the IC only if the content of the envelope is of the Call variant
    async fn relay_call_request_to_ic(&self, message: Message) -> Result<(), IcWsError> {
        let client_request = get_client_request(message)?;
        if let EnvelopeContent::Call { .. } = *client_request.envelope.content {
            let serialized_envelope = serialize(client_request.envelope)?;

            let canister_id = self.canister_id.expect("must be set");

            // relay the envelope to the IC
            self.relay_envelope_to_canister(serialized_envelope, canister_id.clone())
                .await
                .map_err(|e| IcWsError::IcWsProtocol(e.to_string()))?;

            // there is no need to relay the response back to the client as the response to a request to the /call enpoint is not certified by the canister
            // and therefore could be manufactured by the gateway

            trace!("Relayed serialized envelope to canister");
            Ok(())
        } else {
            Err(IcWsError::IcWsProtocol(String::from(
                "Gateway can only relay envelopes with content of Call variant",
            )))
        }
    }

    async fn relay_envelope_to_canister(
        &self,
        serialized_envelope: Vec<u8>,
        canister_id: Principal,
    ) -> Result<(), AgentError> {
        self.agent
            .update_signed(canister_id, serialized_envelope)
            .await?;
        return Ok(());
    }

    async fn relay_canister_message(
        &mut self,
        canister_message: CanisterToClientMessage,
    ) -> Result<(), IcWsError> {
        // relay canister message to client, cbor encoded
        match to_vec(&canister_message) {
            Ok(bytes) => {
                self.send_ws_message_to_client(Message::Binary(bytes))
                    .await?;
                trace!("Message sent to client");
                Ok(())
            },
            Err(e) => Err(IcWsError::IcWsProtocol(format!(
                "Could not serialize canister message. Error: {:?}",
                e
            ))),
        }
    }

    async fn send_ws_message_to_client(&mut self, message: Message) -> Result<(), IcWsError> {
        if let Err(e) = self.ws_write.send(message).await {
            return Err(IcWsError::WebSocket(e.to_string()));
        }
        Ok(())
    }

    async fn inspect_ic_ws_open_message(
        &mut self,
        ws_message: Message,
    ) -> Result<(ClientKey, Principal), IcWsError> {
        let client_request = get_client_request(ws_message)?;
        // the first envelope shall have content of variant Call, which contains canister_id
        if let EnvelopeContent::Call {
            canister_id, arg, ..
        } = &*client_request.envelope.content
        {
            let (ws_open_arguments,): (CanisterWsOpenArguments,) =
                decode_args(arg).map_err(|e| {
                    IcWsError::IcWsProtocol(format!(
                        "arg field of envelope's content has the wrong type: {:?}",
                        e.to_string()
                    ))
                })?;

            let client_principal = client_request.envelope.content.sender().to_owned();
            let client_key = ClientKey::new(client_principal, ws_open_arguments.client_nonce);

            return Ok((client_key, canister_id.to_owned()));
        }
        Err(IcWsError::IcWsProtocol(String::from(
            "first message from client should contain canister_id and arg in envelope's content and should be of Call variant",
        )))
    }
}

fn serialize<S: Serialize>(message: S) -> Result<Vec<u8>, IcWsError> {
    let mut serialized_message = Vec::new();
    let mut serializer = serde_cbor::Serializer::new(&mut serialized_message);
    serializer.self_describe().map_err(|e| {
        IcWsError::WebSocket(format!(
            "could not write self-describe tag to stream. Error: {:?}",
            e.to_string()
        ))
    })?;
    message.serialize(&mut serializer).map_err(|e| {
        IcWsError::WebSocket(format!(
            "could not serialize message. Error: {:?}",
            e.to_string()
        ))
    })?;
    Ok(serialized_message)
}

fn get_client_request<'a>(message: Message) -> Result<ClientRequest<'a>, IcWsError> {
    if let Message::Binary(bytes) = message {
        if let Ok(client_request) = from_slice(&bytes) {
            return Ok(client_request);
        } else {
            return Err(IcWsError::WebSocket(String::from(
                "ws message from client is not of type ClientRequest",
            )));
        }
    }
    Err(IcWsError::WebSocket(String::from(
        "ws message from client is not binary encoded",
    )))
}

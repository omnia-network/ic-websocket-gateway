use crate::{
    canister_methods::{CanisterToClientMessage, CanisterWsOpenArguments, ClientKey},
    canister_poller::IcWsCanisterMessage,
    manager::CanisterPrincipal,
};
use candid::{decode_args, Principal};
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
    sync::mpsc::Receiver,
};
use tokio_tungstenite::{
    tungstenite::{Error, Message},
    WebSocketStream,
};
use tracing::{error, span, trace, warn, Instrument, Level, Span};

/// Message sent by the WS Gateway upon open the (traditional) WebSocket connection
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
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum IcWsSessionState {
    Init,
    Setup(Message),
    Open,
    Closed,
}

/// Possible errors that can occur during an IC WebSocket session
#[derive(Debug, Clone)]
pub enum IcWsError {
    /// Error due to not following the IC WS protocol
    IcWsProtocol(String),
    /// WebSocket error
    WebSocket(String),
    /// Poller error
    Poller(String),
}

/// IC WebSocket session
pub struct ClientSession<S: AsyncRead + AsyncWrite + Unpin> {
    /// Identifier of the client connection
    _client_id: u64,
    /// Key identifying a IC WS session
    pub client_key: Option<ClientKey>,
    /// Principal of the canister the client is connected to
    pub canister_id: Option<CanisterPrincipal>,
    /// Channel used to receive canister updates by the poller
    client_channel_rx: Receiver<IcWsCanisterMessage>,
    /// Sending side of the WS connection with the client
    ws_write: SplitSink<WebSocketStream<S>, Message>,
    /// Receiving side of the WS connection with the client
    ws_read: SplitStream<WebSocketStream<S>>,
    /// Current state of the IC WS session
    session_state: IcWsSessionState,
    /// Agent used to communicate with the IC
    agent: Arc<Agent>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> ClientSession<S> {
    pub async fn init(
        _client_id: u64,
        gateway_principal: Principal,
        client_channel_rx: Receiver<IcWsCanisterMessage>,
        ws_write: SplitSink<WebSocketStream<S>, Message>,
        ws_read: SplitStream<WebSocketStream<S>>,
        agent: Arc<Agent>,
    ) -> Result<Self, IcWsError> {
        let mut client_session = Self {
            _client_id,
            client_key: None,
            canister_id: None,
            client_channel_rx,
            ws_write,
            ws_read,
            session_state: IcWsSessionState::Init,
            agent,
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
            .instrument(Span::current())
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
    pub async fn try_update_state(&mut self) -> Result<Option<IcWsSessionState>, IcWsError> {
        // keep track of the session state before handling the update
        let previous_session_state = self.session_state.clone();
        // the session state may change due to a client update or a canister update
        // blocks the task until either a client update or canister update is received
        select! {
            // 'next' returns None only after the stream is already closed
            // however, if this is the case, the session shall have been closed in the previous call to 'update_state'
            // therefore, we can ignore None
            Some(client_update) = self.ws_read.next() => self.handle_client_update(client_update).await?,
            // in case of a poller error, the poller will terminate immediately, without waiting for the client session handler to cleanup its state and terminate
            // in such a case, the sending side of the channel is dropped and therefore the client session shall return an error
            canister_update = self.client_channel_rx.recv() => self.handle_canister_update(canister_update).await?,
        }
        // if the update resulted in a new session state, return it so that the session handler can act accordingly,
        if self.session_state != previous_session_state {
            return Ok(Some(self.session_state.clone()));
        }
        Ok(None)
    }

    async fn handle_client_update(
        &mut self,
        client_update: Result<Message, Error>,
    ) -> Result<(), IcWsError> {
        match self.session_state {
            IcWsSessionState::Init => {
                let ws_message = self.handle_ws_errors(client_update)?;
                if !ws_message.is_close() {
                    // upon receiving a message while the session is Init, check if the message is valid
                    // if not return an error, otherwise set the session state to Setup
                    // if multiple messages are received while in Init state, 'handle_setup_transition' will
                    // return an error as the client shall not send more than one message while in Init state
                    let setup_state = self.handle_setup_transition(ws_message).await?;
                    self.session_state = setup_state;
                    Ok(())
                } else {
                    trace!("Client disconnected while in Init state");
                    self.session_state = IcWsSessionState::Closed;
                    Ok(())
                }
            },
            IcWsSessionState::Setup(_) => {
                // upon receiving an update while the session is Setup, check if it is due to
                // the client disconnecting without a closing handshake
                let _ = self.handle_ws_errors(client_update)?;
                // upon receiving a message while the session is Setup, discard the message
                // and return an error as the client shall not send a message while in Setup state
                // this implies a bug in the client SDK
                error!("Received client message while in Setup state");
                self.session_state = IcWsSessionState::Closed;
                Err(IcWsError::IcWsProtocol(String::from(
                    "Client shall not send messages while in Setup state",
                )))
            },
            IcWsSessionState::Open => {
                let ws_message = self.handle_ws_errors(client_update)?;
                if !ws_message.is_close() {
                    // upon receiving a message while the session is Open, immediately relay the client messages to the IC
                    // this does not result in a state transition, which shall remain in Open state
                    self.relay_client_message(ws_message)
                        .instrument(Span::current())
                        .await?;
                    Ok(())
                } else {
                    trace!("Client disconnected while in Open state");
                    self.session_state = IcWsSessionState::Closed;
                    Ok(())
                }
            },
            IcWsSessionState::Closed => {
                // upon receiving a message while the session is Closed, discard the message
                // and return an error as this shall not be possible
                // this implies a bug in the WS Gateway
                error!("Received client message while in Closed state");
                Err(IcWsError::IcWsProtocol(String::from(
                    "Client shall not send messages while in Closed state",
                )))
            },
        }
    }

    async fn handle_canister_update(
        &mut self,
        canister_update: Option<IcWsCanisterMessage>,
    ) -> Result<(), IcWsError> {
        match canister_update {
            Some((canister_message, canister_message_span)) => {
                match self.session_state {
                    IcWsSessionState::Init => {
                        // no need to update the session state to Closed as getting to this state shall not be possible
                        // if here, there is a bug in the WS Gateway as the poller is not suppose to relay messages
                        // for uninitialized client sessions
                        unreachable!("Canister shall not send messages while in Init state")
                    },
                    IcWsSessionState::Setup(_) => {
                        let open_state = self
                            .handle_open_transition(canister_message)
                            .instrument(canister_message_span)
                            .await?;
                        self.session_state = open_state;
                        Ok(())
                    },
                    IcWsSessionState::Open => {
                        // once the connection is open, immediately relay the canister messages to the client via the WS
                        // this does not result in a state transition, which shall remain in Open state
                        self.relay_canister_message(canister_message)
                            .instrument(canister_message_span)
                            .await?;
                        Ok(())
                    },
                    IcWsSessionState::Closed => {
                        // upon receiving a message while the session is Closed, discard the message
                        // and return an error as this shall not be possible
                        // this implies a bug in the WS Gateway
                        error!("Received canister message while in Closed state");
                        Err(IcWsError::IcWsProtocol(String::from(
                            "Poller shall not be able tosend messages while the session is in Closed state",
                        )))
                    },
                }
            },
            None => {
                warn!("Poller side of the channel has been dropped. Terminating the session.");
                self.close_ws_session().await?;
                Err(IcWsError::Poller(String::from(
                    "Client session terminated due to poller failure",
                )))
            },
        }
    }

    fn handle_ws_errors(
        &mut self,
        canister_update: Result<Message, Error>,
    ) -> Result<Message, IcWsError> {
        match canister_update {
            Ok(ws_message) => Ok(ws_message),
            Err(e) => {
                // no need to update the session state to Closed as 'update_state' will return an error
                // and the session handler will not call it again
                // set the session state to Closed anyway just for clarity
                self.session_state = IcWsSessionState::Closed;
                Err(IcWsError::WebSocket(format!(
                    "Error receiving message from client: {:?}",
                    e
                )))
            },
        }
    }

    async fn handle_setup_transition(
        &mut self,
        ws_open_message: Message,
    ) -> Result<IcWsSessionState, IcWsError> {
        match self
            .inspect_ic_ws_open_message(ws_open_message.clone())
            .await
        {
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

                // client session is now Setup
                Ok(IcWsSessionState::Setup(ws_open_message))
            },
            // in case of other errors, we report them and terminate the connection handler task
            Err(e) => {
                self.close_ws_session().await?;
                return Err(IcWsError::IcWsProtocol(format!(
                    "IC WS setup failed. Error: {:?}",
                    e
                )));
            },
        }
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

    pub async fn relay_client_message(&self, message: Message) -> Result<(), IcWsError> {
        let client_message_span = span!(
            parent: &Span::current(),
            Level::TRACE,
            "Client Message",
        );

        self.relay_ws_message_to_ic(message)
            .instrument(client_message_span)
            .await
    }

    /// relays the client's request to the IC only if the content of the envelope is of the Call variant
    async fn relay_ws_message_to_ic(&self, message: Message) -> Result<(), IcWsError> {
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

    async fn handle_open_transition(
        &mut self,
        canister_message: CanisterToClientMessage,
    ) -> Result<IcWsSessionState, IcWsError> {
        // if relaying the first canister message to the client succeeds, the client session is Open
        self.relay_canister_message(canister_message).await?;
        Ok(IcWsSessionState::Open)
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

    async fn close_ws_session(&mut self) -> Result<(), IcWsError> {
        if let Err(e) = self.ws_write.close().await {
            return Err(IcWsError::WebSocket(e.to_string()));
        }
        Ok(())
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

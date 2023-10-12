use candid::{CandidType, Principal};
use ic_cdk::api::caller;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::{cell::RefCell, collections::VecDeque, convert::AsRef};

/// The maximum number of messages returned by [ws_get_messages] at each poll.
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 10;

/// The result of [ws_open].
pub type CanisterWsOpenResult = Result<Vec<Principal>, String>;
/// The result of [ws_get_messages].
pub type CanisterWsGetMessagesResult = Result<Vec<CanisterOutputMessage>, String>;
/// The result of [ws_send].
pub type CanisterWsSendResult = Result<(), String>;

/// The arguments for [ws_open].
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsOpenArguments {
    client_nonce: u64,
}

/// The arguments for [ws_get_messages].
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
pub struct CanisterWsGetMessagesArguments {
    nonce: u64,
}

/// Element of the list of messages returned to the WS Gateway after polling.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub struct CanisterOutputMessage {
    key: String, // Key for certificate verification.
    #[serde(with = "serde_bytes")]
    content: Vec<u8>, // The message to be relayed, that contains the application message.
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
/// Contains data about the registered WS Gateway.
struct RegisteredGateway {
    /// The principal of the gateway.
    gateway_principal: Principal,
}

impl RegisteredGateway {
    /// Creates a new instance of RegisteredGateway.
    fn new(gateway_principal: Principal) -> Self {
        Self { gateway_principal }
    }
}

thread_local! {
    /// Maps the client's key to the client metadata
    /* flexible */ static REGISTERED_CANISTERS: RefCell<HashSet<Principal>> = RefCell::new(HashSet::new());
    /// Keeps track of the principal of the WS Gateway which polls the canister
    /* flexible */ static REGISTERED_GATEWAY: RefCell<Option<RegisteredGateway>> = RefCell::new(None);
    /// Keeps track of the messages that have to be sent to the WS Gateway
    /* flexible */ static MESSAGES_FOR_GATEWAY: RefCell<VecDeque<CanisterOutputMessage>> = RefCell::new(VecDeque::new());
    /// Keeps track of the nonce which:
    /// - the WS Gateway uses to specify the first index of the certified messages to be returned when polling
    /// - the client uses as part of the path in the Merkle tree in order to verify the certificate of the messages relayed by the WS Gateway
    /* flexible */ static OUTGOING_MESSAGE_NONCE: RefCell<u64> = RefCell::new(0u64);
}

fn get_outgoing_message_nonce() -> u64 {
    OUTGOING_MESSAGE_NONCE.with(|n| n.borrow().clone())
}

fn increment_outgoing_message_nonce() {
    OUTGOING_MESSAGE_NONCE.with(|n| n.replace_with(|&mut old| old + 1));
}

fn insert_canister_principal(principal: &Principal) {
    REGISTERED_CANISTERS.with(|map| {
        map.borrow_mut().insert(principal.to_owned());
    });
}

fn is_canister_registered(principal: &Principal) -> bool {
    REGISTERED_CANISTERS.with(|map| map.borrow().contains(principal))
}

fn check_registered_canister(principal: &Principal) -> Result<(), String> {
    if !is_canister_registered(principal) {
        return Err(String::from(format!(
            "canister with principal {} is not registered",
            principal
        )));
    }

    Ok(())
}

fn initialize_registered_gateway(gateway_principal: &str) {
    REGISTERED_GATEWAY.with(|p| {
        let gateway_principal =
            Principal::from_text(gateway_principal).expect("invalid gateway principal");
        *p.borrow_mut() = Some(RegisteredGateway::new(gateway_principal));
    });
}

fn get_registered_gateway_principal() -> Principal {
    REGISTERED_GATEWAY.with(|g| {
        g.borrow()
            .expect("gateway should be initialized")
            .gateway_principal
    })
}

fn remove_canister(principal: &Principal) {
    REGISTERED_CANISTERS.with(|map| {
        map.borrow_mut().remove(principal);
    })
}

fn get_message_for_gateway_key(gateway_principal: Principal, nonce: u64) -> String {
    gateway_principal.to_string() + "_" + &format!("{:0>20}", nonce.to_string())
}

fn get_messages_for_gateway_range(gateway_principal: Principal, nonce: u64) -> (usize, usize) {
    MESSAGES_FOR_GATEWAY.with(|m| {
        let queue_len = m.borrow().len();

        if nonce == 0 && queue_len > 0 {
            // this is the case in which the poller on the gateway restarted
            // the range to return is end:last index and start: max(end - MAX_NUMBER_OF_RETURNED_MESSAGES, 0)
            let start_index = if queue_len > MAX_NUMBER_OF_RETURNED_MESSAGES {
                queue_len - MAX_NUMBER_OF_RETURNED_MESSAGES
            } else {
                0
            };

            return (start_index, queue_len);
        }

        // smallest key used to determine the first message from the queue which has to be returned to the WS Gateway
        let smallest_key = get_message_for_gateway_key(gateway_principal, nonce);
        // partition the queue at the message which has the key with the nonce specified as argument to get_cert_messages
        let start_index = m.borrow().partition_point(|x| x.key < smallest_key);
        // message at index corresponding to end index is excluded
        let mut end_index = queue_len;
        if end_index - start_index > MAX_NUMBER_OF_RETURNED_MESSAGES {
            end_index = start_index + MAX_NUMBER_OF_RETURNED_MESSAGES;
        }
        (start_index, end_index)
    })
}

fn get_messages_for_gateway(start_index: usize, end_index: usize) -> Vec<CanisterOutputMessage> {
    MESSAGES_FOR_GATEWAY.with(|m| {
        let mut messages: Vec<CanisterOutputMessage> = Vec::with_capacity(end_index - start_index);
        for index in start_index..end_index {
            messages.push(m.borrow().get(index).unwrap().clone());
        }
        messages
    })
}

/// Gets the messages in MESSAGES_FOR_GATEWAY starting from the one with the specified nonce
fn get_messages(gateway_principal: Principal, nonce: u64) -> CanisterWsGetMessagesResult {
    let (start_index, end_index) = get_messages_for_gateway_range(gateway_principal, nonce);
    Ok(get_messages_for_gateway(start_index, end_index))
}

fn is_registered_gateway(principal: Principal) -> bool {
    let registered_gateway_principal = get_registered_gateway_principal();
    return registered_gateway_principal == principal;
}

/// Checks if the caller of the method is the same as the one that was registered during the initialization of the CDK
fn check_is_registered_gateway(input_principal: Principal) -> Result<(), String> {
    let gateway_principal = get_registered_gateway_principal();
    // check if the caller is the same as the one that was registered during the initialization of the CDK
    if gateway_principal != input_principal {
        return Err(String::from(
            "caller is not the gateway that has been registered during CDK initialization",
        ));
    }
    Ok(())
}

/// Internal function used to put the messages in the outgoing messages queue and certify them.
fn _ws_send(msg_bytes: Vec<u8>) -> CanisterWsSendResult {
    // get the principal of the gateway that is polling the canister
    let gateway_principal = get_registered_gateway_principal();

    // the nonce in key is used by the WS Gateway to determine the message to start in the polling iteration
    // the key is also passed to the client in order to validate the body of the certified message
    let outgoing_message_nonce = get_outgoing_message_nonce();
    let key = get_message_for_gateway_key(gateway_principal, outgoing_message_nonce);

    // increment the nonce for the next message
    increment_outgoing_message_nonce();

    MESSAGES_FOR_GATEWAY.with(|m| {
        // messages in the queue are inserted with contiguous and increasing nonces
        // (from beginning to end of the queue) as ws_send is called sequentially, the nonce
        // is incremented by one in each call, and the message is pushed at the end of the queue
        m.borrow_mut().push_back(CanisterOutputMessage {
            content: msg_bytes,
            key,
        });
    });
    Ok(())
}

/// Parameters for the IC WebSocket CDK initialization. For default parameters and simpler initialization, use [`WsInitParams::new`].
#[derive(Clone)]
pub struct WsInitParams {
    /// The principal of the WS Gateway that will be polling the canister.
    pub gateway_principal: String,
}

impl WsInitParams {
    /// Creates a new instance of WsInitParams, with default interval values.
    pub fn new(gateway_principal: String) -> Self {
        Self { gateway_principal }
    }
}

/// Initialize the CDK by setting the callback handlers and the **principal** of the WS Gateway that
/// will be polling the canister.
///
/// Under the hood, an interval (**60 seconds**) is started using [ic_cdk_timers::set_timer]
/// to check if the WS Gateway is still alive.
pub fn init(params: WsInitParams) {
    // set the principal of the (only) WS Gateway that will be polling the canister
    initialize_registered_gateway(&params.gateway_principal);
}

/// Handles the WS connection open event sent by the client and relayed by the Gateway.
pub fn ws_open() -> CanisterWsOpenResult {
    let client_principal = caller();

    // only gateway can open a connection
    if !is_registered_gateway(client_principal) {
        return Err(String::from("caller is not the registered gateway"));
    } else {
        // TODO: gateway restarted, respond with all registered canisters
        Ok(vec![])
    }
}

/// Returns messages to the WS Gateway in response of a polling iteration.
pub fn ws_get_messages(args: CanisterWsGetMessagesArguments) -> CanisterWsGetMessagesResult {
    // check if the caller of this method is the WS Gateway that has been set during the initialization of the SDK
    let gateway_principal = caller();
    check_is_registered_gateway(gateway_principal)?;

    get_messages(gateway_principal, args.nonce)
}

/// Sends a message to the client. The message must already be serialized, using a method of your choice, like Candid or CBOR.
///
/// Under the hood, the message is serialized and certified, and then it is added to the queue of messages
/// that the WS Gateway will poll in the next iteration.
pub fn ws_send(msg_bytes: Vec<u8>) -> CanisterWsSendResult {
    _ws_send(msg_bytes)
}

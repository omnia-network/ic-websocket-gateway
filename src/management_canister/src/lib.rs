use candid::{CandidType, Principal};
use ic_cdk::api::caller;
use ic_cdk_macros::{init, post_upgrade, query, update};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::{cell::RefCell, collections::VecDeque, convert::AsRef};

/// The maximum number of messages returned by [get_new_registered_canisters] at each poll.
const MAX_NUMBER_OF_RETURNED_MESSAGES: usize = 10;

const GATEWAY_PRINCIPAL: &str = "be6t5-k6m66-cifvj-yibal-eornr-h7wh4-jtj2p-yhvgs-p7str-dn7sq-xae";

type RegisterGatewayResult = Result<Vec<Principal>, String>;
type CanisterRegistrationResult = Result<(), String>;
type CanisterDeregistrationResult = Result<(), String>;
/// The result of [get_new_registered_canisters].
type GetNewRegisteredCanistersResult = Result<Vec<CanisterRegistration>, String>;
/// The result of [ws_send].
type SendNewRegisteredCanisterResult = Result<(), String>;

/// The arguments for [get_new_registered_canisters].
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug)]
struct GetNewRegisteredCanistersArgs {
    nonce: u64,
}

/// Element of the list of messages returned to the WS Gateway after polling.
#[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq)]
struct CanisterRegistration {
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
    /* flexible */ static MESSAGES_FOR_GATEWAY: RefCell<VecDeque<CanisterRegistration>> = RefCell::new(VecDeque::new());
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

fn get_registered_canisters() -> Vec<Principal> {
    REGISTERED_CANISTERS.with(|map| {
        map.borrow().iter().fold(Vec::new(), |mut v, principal| {
            v.push(principal.to_owned());
            v
        })
    })
}

fn is_canister_registered(principal: &Principal) -> bool {
    REGISTERED_CANISTERS.with(|map| map.borrow().contains(principal))
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

fn get_messages_for_gateway(start_index: usize, end_index: usize) -> Vec<CanisterRegistration> {
    MESSAGES_FOR_GATEWAY.with(|m| {
        let mut messages: Vec<CanisterRegistration> = Vec::with_capacity(end_index - start_index);
        for index in start_index..end_index {
            messages.push(m.borrow().get(index).unwrap().clone());
        }
        messages
    })
}

/// Gets the messages in MESSAGES_FOR_GATEWAY starting from the one with the specified nonce
fn get_messages(gateway_principal: Principal, nonce: u64) -> GetNewRegisteredCanistersResult {
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

#[init]
fn init() {
    // set the principal of the (only) WS Gateway that will be polling the canister
    initialize_registered_gateway(GATEWAY_PRINCIPAL);
}

#[post_upgrade]
fn post_upgrade() {
    init();
}

#[update]
fn register_gateway() -> RegisterGatewayResult {
    let client_principal = caller();

    // only gateway can open a connection
    if !is_registered_gateway(client_principal) {
        return Err(String::from("caller is not the registered gateway"));
    } else {
        // gateway restarted, respond with all registered canisters
        Ok(get_registered_canisters())
    }
}

#[update]
fn register_canister() -> CanisterRegistrationResult {
    let principal = caller();
    if !is_canister_registered(&principal) {
        insert_canister_principal(&principal);
        // TODO: send update to gateway
        Ok(())
    } else {
        Err(format!(
            "Canister with principal: {:?} already registered",
            principal,
        ))
    }
}

#[update]
fn deregister_canister() -> CanisterDeregistrationResult {
    let principal = caller();
    if is_canister_registered(&principal) {
        remove_canister(&principal);
        // TODO: send update to gateway
        Ok(())
    } else {
        Err(format!(
            "Canister with principal: {:?} is not registered",
            principal,
        ))
    }
}

/// Returns messages to the WS Gateway in response of a polling iteration.
#[query]
fn get_new_registered_canisters(
    args: GetNewRegisteredCanistersArgs,
) -> GetNewRegisteredCanistersResult {
    // check if the caller of this method is the WS Gateway that has been set during the initialization of the SDK
    let gateway_principal = caller();
    check_is_registered_gateway(gateway_principal)?;

    get_messages(gateway_principal, args.nonce)
}

pub fn send_new_registered_canister(msg_bytes: Vec<u8>) -> SendNewRegisteredCanisterResult {
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
        m.borrow_mut().push_back(CanisterRegistration {
            content: msg_bytes,
            key,
        });
    });
    Ok(())
}

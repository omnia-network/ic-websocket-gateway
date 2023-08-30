#[cfg(test)]
// !!! tests have to be run using "cargo test -- --test-threads=1" !!!
// running them cuncurrently results in an error as multiple instances of GatewayServer use the same address
mod tests {
    use ic_agent::export::Principal;
    use ic_identity::{get_identity_from_key_pair, load_key_pair};
    use serde::Serialize;
    use serde_cbor::Serializer;
    use std::net::TcpStream;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use websocket::sync::Client;
    use websocket::ClientBuilder;

    use crate::canister_methods::CanisterOpenMessageContent;
    use crate::canister_methods::RelayedClientMessage;
    use crate::client_connection_handler::IcWsError;
    use crate::client_connection_handler::WsConnectionState;
    use crate::create_data_dir;
    use crate::gateway_server::GatewaySession;
    use crate::GatewayServer;

    fn get_mock_websocket_client(addr: &str) -> Client<TcpStream> {
        ClientBuilder::new(&format!("ws://{}", addr))
            .unwrap()
            .connect_insecure()
            .expect("Error connecting to WebSocket server.")
    }

    fn cbor_serialize<T: Serialize>(m: T) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut serializer = Serializer::new(&mut bytes);
        serializer.self_describe().unwrap();
        m.serialize(&mut serializer).unwrap();
        bytes
    }

    fn candid_serialize<T: CandidType>(m: T) -> Vec<u8> {
        encode_one(m).expect("Candid serialization should not fail")
    }

    fn get_valid_signature() -> Vec<u8> {
        vec![
            182, 213, 168, 36, 71, 219, 76, 54, 18, 192, 209, 98, 164, 87, 237, 175, 233, 118, 47,
            39, 10, 188, 252, 3, 110, 212, 121, 163, 112, 222, 186, 190, 185, 51, 85, 78, 148, 17,
            12, 229, 11, 181, 84, 117, 168, 61, 57, 122, 70, 5, 39, 109, 171, 153, 194, 146, 215,
            220, 6, 56, 9, 157, 126, 4,
        ]
    }

    fn get_valid_client_key() -> Vec<u8> {
        vec![
            229, 173, 124, 88, 70, 98, 66, 88, 106, 214, 233, 97, 108, 15, 187, 54, 121, 43, 50,
            45, 131, 52, 17, 59, 72, 46, 186, 105, 141, 71, 119, 203,
        ]
    }

    fn get_valid_serialized_canister_open_message_content() -> Vec<u8> {
        vec![
            217, 217, 247, 162, 107, 99, 97, 110, 105, 115, 116, 101, 114, 95, 105, 100, 74, 128,
            0, 0, 0, 0, 16, 0, 1, 1, 1, 106, 99, 108, 105, 101, 110, 116, 95, 107, 101, 121, 88,
            32, 229, 173, 124, 88, 70, 98, 66, 88, 106, 214, 233, 97, 108, 15, 187, 54, 121, 43,
            50, 45, 131, 52, 17, 59, 72, 46, 186, 105, 141, 71, 119, 203,
        ]
    }

    fn get_valid_serialized_relayed_client_message() -> Vec<u8> {
        let message = RelayedClientMessage {
            content: get_valid_serialized_canister_open_message_content(),
            sig: get_valid_signature(),
        };
        candid_serialize(message)
    }

    async fn start_client_server() -> (Client<TcpStream>, GatewayServer) {
        create_data_dir().unwrap();

        let gateway_addr = String::from("127.0.0.1:8080");
        let subnet_addr = String::from("http://127.0.0.1:4943");
        let key_pair = load_key_pair("./data/key_pair").unwrap();
        let identity = get_identity_from_key_pair(key_pair);

        let (metrics_channel_tx, _metrics_channel_rx) = mpsc::channel(100);

        let gateway_server = GatewayServer::new(
            gateway_addr.clone(),
            subnet_addr,
            identity,
            metrics_channel_tx,
        )
        .await;
        gateway_server.start_accepting_incoming_connections(None);
        // delay the client connection in order to give the server time to start listening for incoming connections
        tokio::time::sleep(Duration::from_millis(100)).await;
        let client = get_mock_websocket_client(&gateway_addr);
        (client, gateway_server)
    }

    // #[tokio::test(flavor = "multi_thread")]
    // async fn client_should_send_binary_first_message() {
    //     let (mut client, mut server) = start_client_server().await;

    //     // client sends the first message as text to the server right after connecting
    //     client
    //         .send_message(&websocket::OwnedMessage::Text(String::from(
    //             "first message",
    //         )))
    //         .unwrap();

    //     let res = server.recv_from_client_connection_handler().await;

    //     let ws_connection_state = res.expect("should not be None");
    //     if let WsConnectionState::Error(IcWsError::Initialization(e)) = ws_connection_state {
    //         return assert_eq!(
    //             e,
    //             String::from("Client did not follow IC WebSocket establishment protocol: \"first message from client should be binary encoded\"")
    //         );
    //     }
    //     panic!("ws_connection_state does not have the expected type");
    // }

    // #[tokio::test(flavor = "multi_thread")]
    // async fn client_should_send_binary_first_message_of_correct_type() {
    //     let (mut client, mut server) = start_client_server().await;

    //     // client sends the first message as binary to the server right after connecting but serialized from a type which is not RelayedClientMessage
    //     client
    //         .send_message(&websocket::OwnedMessage::Binary(Vec::<u8>::new()))
    //         .unwrap();

    //     let res = server.recv_from_client_connection_handler().await;

    //     let ws_connection_state = res.expect("should not be None");
    //     if let WsConnectionState::Error(IcWsError::Initialization(e)) = ws_connection_state {
    //         return assert_eq!(
    //             e,
    //             String::from("Client did not follow IC WebSocket establishment protocol: \"first message is not of type RelayedClientMessage\"")
    //         );
    //     }
    //     panic!("ws_connection_state does not have the expected type");
    // }

    // #[tokio::test(flavor = "multi_thread")]
    // async fn first_message_content_should_be_of_right_type() {
    //     let (mut client, mut server) = start_client_server().await;

    //     // client sends the first message as binary to the server right after connecting, serialized from the type RelayedClientMessage
    //     // but with content not of type CanisterFirstMessageContent
    //     let message = RelayedClientMessage {
    //         content: Vec::new(),
    //         sig: Vec::new(),
    //     };
    //     let serialized_message = serialize(message);

    //     client
    //         .send_message(&websocket::OwnedMessage::Binary(serialized_message))
    //         .unwrap();

    //     let res = server.recv_from_client_connection_handler().await;

    //     let ws_connection_state = res.expect("should not be None");
    //     if let WsConnectionState::Error(IcWsError::Initialization(e)) = ws_connection_state {
    //         return assert_eq!(
    //             e,
    //             String::from("Client did not follow IC WebSocket establishment protocol: \"content of first message is not of type CanisterFirstMessageContent\"")
    //         );
    //     }
    //     panic!("ws_connection_state does not have the expected type");
    // }

    // #[tokio::test(flavor = "multi_thread")]
    // async fn first_message_should_contain_valid_signature() {
    //     let (mut client, mut server) = start_client_server().await;

    //     // client sends the first message as binary to the server right after connecting, serialized from the type RelayedClientMessage
    //     // but with an invalid signature
    //     let content = CanisterFirstMessageContent {
    //         client_key: Vec::new(),
    //         canister_id: Principal::anonymous(),
    //     };
    //     let serialized_content = serialize(content);

    //     let message = RelayedClientMessage {
    //         content: serialized_content,
    //         sig: Vec::new(),
    //     };
    //     let serialized_message = serialize(message);

    //     client
    //         .send_message(&websocket::OwnedMessage::Binary(serialized_message))
    //         .unwrap();

    //     let res = server.recv_from_client_connection_handler().await;

    //     let ws_connection_state = res.expect("should not be None");
    //     if let WsConnectionState::Error(IcWsError::Initialization(e)) = ws_connection_state {
    //         return assert_eq!(
    //             e,
    //             String::from("Client did not follow IC WebSocket establishment protocol: \"first message does not contain a valid signature\"")
    //         );
    //     }
    //     panic!("ws_connection_state does not have the expected type");
    // }

    // #[tokio::test(flavor = "multi_thread")]
    // async fn first_message_should_contain_valid_public_key() {
    //     let (mut client, mut server) = start_client_server().await;

    //     // client sends the first message as binary to the server right after connecting, serialized from the type RelayedClientMessage
    //     // but with an invalid public key (client_key)
    //     let content = CanisterFirstMessageContent {
    //         client_key: Vec::new(),
    //         canister_id: Principal::anonymous(),
    //     };
    //     let serialized_content = serialize(content);

    //     let valid_signature = get_valid_signature();

    //     let message = RelayedClientMessage {
    //         content: serialized_content,
    //         sig: valid_signature,
    //     };
    //     let serialized_message = serialize(message);

    //     client
    //         .send_message(&websocket::OwnedMessage::Binary(serialized_message))
    //         .unwrap();

    //     let res = server.recv_from_client_connection_handler().await;

    //     let ws_connection_state = res.expect("should not be None");
    //     if let WsConnectionState::Error(IcWsError::Initialization(e)) = ws_connection_state {
    //         return assert_eq!(
    //             e,
    //             String::from("Client did not follow IC WebSocket establishment protocol: \"first message does not contain a valid public key\"")
    //         );
    //     }
    //     panic!("ws_connection_state does not have the expected type");
    // }

    // #[tokio::test(flavor = "multi_thread")]
    // async fn signature_should_verify_against_public_key() {
    //     let (mut client, mut server) = start_client_server().await;

    //     // client sends the first message as binary to the server right after connecting, serialized from the type RelayedClientMessage
    //     // but the client's signature does not verify the message against the public key
    //     let valid_client_key = get_valid_client_key();
    //     let content = CanisterFirstMessageContent {
    //         client_key: valid_client_key,
    //         canister_id: Principal::anonymous(),
    //     };
    //     let serialized_content = serialize(content);

    //     let valid_signature = get_valid_signature();

    //     let message = RelayedClientMessage {
    //         content: serialized_content,
    //         sig: valid_signature,
    //     };
    //     let serialized_message = serialize(message);

    //     client
    //         .send_message(&websocket::OwnedMessage::Binary(serialized_message))
    //         .unwrap();

    //     let res = server.recv_from_client_connection_handler().await;

    //     let ws_connection_state = res.expect("should not be None");
    //     if let WsConnectionState::Error(IcWsError::Initialization(e)) = ws_connection_state {
    //         return assert_eq!(
    //             e,
    //             String::from("Client did not follow IC WebSocket establishment protocol: \"client's signature does not verify against public key\"")
    //         );
    //     }
    //     panic!("ws_connection_state does not have the expected type");
    // }

    #[tokio::test(flavor = "multi_thread")]
    async fn gets_gateway_session() {
        let (mut client, mut server) = start_client_server().await;

        // client follows the IC WS connection establishment correctly
        let valid_serialized_message = get_valid_serialized_relayed_client_message();

        client
            .send_message(&websocket::OwnedMessage::Binary(valid_serialized_message))
            .unwrap();

        let res = server.recv_from_client_connection_handler().await;

        let expected_client_id = 0 as u64;
        let expected_client_key = get_valid_client_key();
        let expected_canister_id =
            Principal::from_text("bkyz2-fmaaa-aaaaa-qaaaq-cai").expect("not a valid principal");
        let expected_nonce = 0 as u64;

        let ws_connection_state = res.expect("should not be None");
        if let WsConnectionState::Established(GatewaySession {
            client_id,
            client_key,
            canister_id,
            nonce,
            ..  // ignore message_for_client_tx as it does does not implement Eq
        }) = ws_connection_state
        {
            return assert_eq!(
                client_id == expected_client_id
                    && client_key == expected_client_key
                    && canister_id == expected_canister_id
                    && nonce == expected_nonce,
                true
            );
        }
        panic!("ws_connection_state does not have the expected type");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn detects_closed_connection_after_session_established() {
        let (mut client, mut server) = start_client_server().await;

        // client follows the IC WS connection establishment correctly and then disconnects
        let valid_serialized_message = get_valid_serialized_relayed_client_message();

        client
            .send_message(&websocket::OwnedMessage::Binary(valid_serialized_message))
            .unwrap();

        let _res = server.recv_from_client_connection_handler().await; // ignore gateway session returned after open message

        // close client connection
        client.shutdown().expect("client should have been running");

        let res = server.recv_from_client_connection_handler().await;

        let expected_client_id = 0;

        let ws_connection_state = res.expect("should not be None");
        if let WsConnectionState::Closed(client_id) = ws_connection_state {
            return assert_eq!(client_id, expected_client_id);
        }
        panic!("ws_connection_state does not have the expected type");
    }
}

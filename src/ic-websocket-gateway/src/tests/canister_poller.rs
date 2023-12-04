mod test {
    use crate::{canister_methods, canister_poller::get_nonce_from_message};
    use candid::{CandidType, Principal};
    use serde::{Deserialize, Serialize};

    /// List of messages returned to the WS Gateway after polling.
    #[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
    struct CanisterOutputCertifiedMessages {
        messages: Vec<CanisterOutputMessage>, // List of messages.
        #[serde(with = "serde_bytes")]
        cert: Vec<u8>, // cert+tree constitute the certificate for all returned messages.
        #[serde(with = "serde_bytes")]
        tree: Vec<u8>, // cert+tree constitute the certificate for all returned messages.
        is_end_of_queue: bool, // Whether the end of the messages queue has been reached.
    }

    impl CanisterOutputCertifiedMessages {
        fn empty() -> Self {
            Self {
                messages: Vec::default(),
                cert: Vec::default(),
                tree: Vec::default(),
                is_end_of_queue: false,
            }
        }

        fn mock_n(n: usize) -> Self {
            let messages = (0..n).map(|i| CanisterOutputMessage::mock(i)).collect();
            Self {
                messages,
                cert: Vec::default(),
                tree: Vec::default(),
                is_end_of_queue: false,
            }
        }
    }

    /// Element of the list of messages returned to the WS Gateway after polling.
    #[derive(CandidType, Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
    struct CanisterOutputMessage {
        pub(crate) client_key: ClientKey, // The client that the gateway will forward the message to or that sent the message.
        pub(crate) key: String,           // Key for certificate verification.
        #[serde(with = "serde_bytes")]
        pub(crate) content: Vec<u8>, // The message to be relayed, that contains the application message.
    }

    impl CanisterOutputMessage {
        fn mock(nonce: usize) -> Self {
            Self {
                client_key: ClientKey::mock(),
                key: format!("_{}", nonce),
                content: Vec::default(),
            }
        }
    }

    #[derive(CandidType, Clone, Deserialize, Serialize, Eq, PartialEq, Debug, Hash)]
    struct ClientKey {
        pub(crate) client_principal: ClientPrincipal,
        pub(crate) client_nonce: u64,
    }

    impl ClientKey {
        fn mock() -> Self {
            Self {
                client_principal: Principal::anonymous(),
                client_nonce: 0,
            }
        }
    }

    type ClientPrincipal = Principal;

    #[tokio::test]
    async fn should_poll_and_relay() {
        let mut server = mockito::Server::new();
        let url = server.url();

        let body = CanisterOutputCertifiedMessages::mock_n(10);
        let body = candid::encode_one(&body).unwrap();

        let mock = server
            .mock("GET", "/ws_get_messages")
            .with_body(body)
            .create();

        match canister_methods::mock_ws_get_messages(url).await {
            Ok(res) => {
                assert_eq!(res.messages.len(), 10);
                for (i, msg) in res.messages.iter().enumerate() {
                    assert_eq!(
                        i,
                        get_nonce_from_message(&msg.key).expect("Failed to get nonce") as usize
                    )
                }
            },
            Err(e) => panic!("Failed to poll: {:?}", e),
        }

        mock.assert();
    }
}

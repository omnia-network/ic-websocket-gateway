mod test {
    use crate::canister_methods;
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
            let messages = (0..n).map(|_| CanisterOutputMessage::mock()).collect();
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
        fn mock() -> Self {
            Self {
                client_key: ClientKey::mock(),
                key: String::default(),
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
    async fn test_canister_poller() {
        let mut server = mockito::Server::new();
        let url = server.url();

        let body = CanisterOutputCertifiedMessages::mock_n(1);
        let body = candid::encode_one(&body).unwrap();

        let mock = server
            .mock("GET", "/ws_get_messages")
            .with_body(body)
            .create();

        let _res = canister_methods::mock_ws_get_messages(url).await;

        mock.assert();
    }
}

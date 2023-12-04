mod test {
    use crate::{
        canister_methods::{
            self, CanisterOutputCertifiedMessages, CanisterOutputMessage, ClientKey,
        },
        canister_poller::get_nonce_from_message,
    };
    use candid::Principal;

    impl CanisterOutputCertifiedMessages {
        fn empty() -> Self {
            Self {
                messages: Vec::default(),
                cert: Vec::default(),
                tree: Vec::default(),
                is_end_of_queue: Some(false),
            }
        }

        fn mock_n(n: usize) -> Self {
            let messages = (0..n).map(|i| CanisterOutputMessage::mock(i)).collect();
            Self {
                messages,
                cert: Vec::default(),
                tree: Vec::default(),
                is_end_of_queue: Some(false),
            }
        }
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

    impl ClientKey {
        fn mock() -> Self {
            Self {
                client_principal: Principal::anonymous(),
                client_nonce: 0,
            }
        }
    }

    #[tokio::test]
    async fn should_poll_and_validate_nonces() {
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

mod test {
    use crate::{
        canister_methods::{
            self, CanisterOutputCertifiedMessages, CanisterOutputMessage, ClientKey,
        },
        canister_poller::get_nonce_from_message,
    };
    use candid::Principal;

    impl CanisterOutputCertifiedMessages {
        fn mock_n(n: usize) -> Self {
            let messages = (0..n).map(|i| CanisterOutputMessage::mock(i)).collect();
            Self {
                messages,
                cert: Vec::default(),
                tree: Vec::default(),
                is_end_of_queue: Some(false),
            }
        }

        fn mock_n_with_key_error(n: usize) -> Self {
            let mut canister_msgs = CanisterOutputCertifiedMessages::mock_n(n - 1);
            canister_msgs
                .messages
                .push(CanisterOutputMessage::mock_with_key_error());
            canister_msgs
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

        fn mock_with_key_error() -> Self {
            Self {
                client_key: ClientKey::mock(),
                key: "_not-a-u64".to_string(),
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

    #[cfg(test)]
    fn start_mock_server(
        body: Vec<u8>,
        path: &str,
    ) -> (mockito::ServerGuard, mockito::Mock, String) {
        let mut server = mockito::Server::new();
        let url = server.url();
        let mock = server.mock("GET", path).with_body(body).create();

        (server, mock, url)
    }

    #[tokio::test]
    async fn should_poll_and_validate_nonces() {
        let msg_count = 10;
        let body = CanisterOutputCertifiedMessages::mock_n(msg_count);
        let body = candid::encode_one(&body).unwrap();
        let path = "/ws_get_messages";

        let (_server, mock, url) = start_mock_server(body, path);

        match canister_methods::mock_ws_get_messages(url).await {
            Ok(res) => {
                assert_eq!(res.messages.len(), msg_count);
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

    #[tokio::test]
    async fn should_poll_and_fail_to_validate_last_nonce() {
        let msg_count = 10;
        let body = CanisterOutputCertifiedMessages::mock_n_with_key_error(msg_count);
        let body = candid::encode_one(&body).unwrap();
        let path = "/ws_get_messages";

        let (_server, mock, url) = start_mock_server(body, path);

        match canister_methods::mock_ws_get_messages(url).await {
            Ok(res) => {
                assert_eq!(res.messages.len(), msg_count);
                for (i, msg) in res.messages.iter().enumerate() {
                    if i == msg_count - 1 {
                        assert!(get_nonce_from_message(&msg.key).is_err());
                    } else {
                        assert_eq!(
                            i,
                            get_nonce_from_message(&msg.key).expect("Failed to get nonce") as usize
                        )
                    }
                }
            },
            Err(e) => panic!("Failed to poll: {:?}", e),
        }

        mock.assert();
    }
}

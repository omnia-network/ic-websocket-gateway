#[cfg(test)]
mod test {
    use std::{sync::Arc, time::Duration};

    use crate::{
        canister_methods::{
            self, CanisterOutputCertifiedMessages, CanisterOutputMessage,
            CanisterWsGetMessagesArguments, ClientKey,
        },
        canister_poller::{get_nonce_from_message, CanisterPoller, IcWsCanisterMessage},
        manager::{GatewaySharedState, GatewayState},
    };
    use candid::Principal;
    use ic_agent::{agent::http_transport::ReqwestTransport, Agent};
    use tokio::sync::mpsc::{self, Receiver, Sender};
    use tracing::Span;

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
    async fn start_mock_server(
        body: Vec<u8>,
        path: &str,
        port: u64,
    ) -> (mockito::Server, mockito::Mock) {
        let mut server = mockito::Server::new_with_port(port as u16);
        let mock = server
            .mock("GET", path)
            .with_body(body)
            .create_async()
            .await;

        (server, mock)
    }

    fn create_poller(
        polling_interval_ms: u64,
        client_channel_tx: Sender<IcWsCanisterMessage>,
    ) -> CanisterPoller {
        let gateway_shared_state: GatewaySharedState = Arc::new(GatewayState::new());

        let poller_state = gateway_shared_state
            .insert_client_channel_and_get_new_poller_state(
                Principal::anonymous(),
                ClientKey::mock(),
                client_channel_tx,
                Span::current(),
            )
            .expect("must be some");

        CanisterPoller::new(
            Arc::new(
                Agent::builder()
                    .with_transport(ReqwestTransport::create("http://127.0.0.1:4943").unwrap())
                    .build()
                    .unwrap(),
            ),
            Principal::anonymous(),
            poller_state,
            gateway_shared_state,
            polling_interval_ms,
        )
    }

    #[tokio::test]
    async fn should_poll_and_validate_nonces() {
        let port = 51558;
        let msg_count = 10;
        let body = CanisterOutputCertifiedMessages::mock_n(msg_count);
        let body = candid::encode_one(&body).unwrap();
        let path = "/ws_get_messages";
        let (_server, mock) = start_mock_server(body, path, port).await;

        let agent = Agent::builder()
            .with_transport(ReqwestTransport::create("http://127.0.0.1:4943").unwrap())
            .build()
            .unwrap();
        let args = CanisterWsGetMessagesArguments { nonce: port };

        match canister_methods::ws_get_messages(&agent, &Principal::anonymous(), args).await {
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
        let port = 51559;
        let msg_count = 10;
        let body = CanisterOutputCertifiedMessages::mock_n_with_key_error(msg_count);
        let body = candid::encode_one(&body).unwrap();
        let path = "/ws_get_messages";
        let (_server, mock) = start_mock_server(body, path, port).await;

        let agent = Agent::builder()
            .with_transport(ReqwestTransport::create("http://127.0.0.1:4943").unwrap())
            .build()
            .unwrap();
        let args = CanisterWsGetMessagesArguments { nonce: port };

        match canister_methods::ws_get_messages(&agent, &Principal::anonymous(), args).await {
            Ok(res) => {
                assert_eq!(res.messages.len(), msg_count);
                for (i, msg) in res.messages.iter().enumerate() {
                    if i != msg_count - 1 {
                        assert_eq!(
                            i,
                            get_nonce_from_message(&msg.key).expect("Failed to get nonce") as usize
                        )
                    } else {
                        assert!(get_nonce_from_message(&msg.key).is_err());
                    }
                }
            },
            Err(e) => panic!("Failed to poll: {:?}", e),
        }

        mock.assert();
    }

    #[tokio::test]
    async fn should_sleep_after_relaying() {
        let port = 51560;
        let msg_count = 10;
        let body = CanisterOutputCertifiedMessages::mock_n(msg_count);
        let body = candid::encode_one(&body).unwrap();
        let path = "/ws_get_messages";
        let (_server, mock) = start_mock_server(body, path, port).await;

        let polling_interval_ms = 100;
        let (client_channel_tx, mut client_channel_rx): (
            Sender<IcWsCanisterMessage>,
            Receiver<IcWsCanisterMessage>,
        ) = mpsc::channel(100);

        let mut poller = create_poller(polling_interval_ms, client_channel_tx);
        poller.message_nonce = port;
        let start_polling_instant = tokio::time::Instant::now();
        tokio::spawn(async move {
            poller.poll_and_relay().await.expect("Failed to poll");
            let end_polling_instant = tokio::time::Instant::now();
            let elapsed = end_polling_instant - start_polling_instant;
            println!("Elapsed: {:?}", elapsed);
            assert!(
                elapsed > Duration::from_millis(polling_interval_ms)
                    && elapsed
                        < Duration::from_millis((1.1 * polling_interval_ms as f64).round() as u64)
            );
        });

        let mut i = 0;
        while let Some((msg, _)) = client_channel_rx.recv().await {
            assert_eq!(i, get_nonce_from_message(&msg.key).unwrap());
            i += 1;
        }

        mock.assert();
    }
}

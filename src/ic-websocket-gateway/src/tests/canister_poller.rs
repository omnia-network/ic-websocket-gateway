#[cfg(test)]
mod test {
    use crate::{
        canister_methods::{
            self, CanisterOutputCertifiedMessages, CanisterOutputMessage,
            CanisterWsGetMessagesArguments, ClientKey,
        },
        canister_poller::{get_nonce_from_message, CanisterPoller, IcWsCanisterMessage},
        manager::{GatewaySharedState, GatewayState},
    };
    use candid::Principal;
    use futures_util::join;
    use ic_agent::{agent::http_transport::ReqwestTransport, Agent};
    use lazy_static::lazy_static;
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tokio::sync::mpsc::{self, Receiver, Sender};
    use tracing::Span;

    impl CanisterOutputCertifiedMessages {
        fn serialize(&self) -> Vec<u8> {
            candid::encode_one(self).unwrap()
        }

        fn mock_n(n: usize, base_nonce: usize) -> Self {
            let messages = (0..n)
                .map(|off_nonce| CanisterOutputMessage::mock(base_nonce + off_nonce))
                .collect();
            Self {
                messages,
                cert: Vec::default(),
                tree: Vec::default(),
                is_end_of_queue: Some(true),
            }
        }

        fn mock_n_with_key_error(n: usize, base_nonce: usize) -> Self {
            let mut canister_msgs = CanisterOutputCertifiedMessages::mock_n(n - 1, base_nonce);
            canister_msgs
                .messages
                .push(CanisterOutputMessage::mock_with_key_error());
            canister_msgs
        }

        fn mock_n_with_not_end_of_queue(n: usize, base_nonce: usize) -> Self {
            let mut canister_msgs = CanisterOutputCertifiedMessages::mock_n(n, base_nonce);
            canister_msgs.is_end_of_queue = Some(false);
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

    lazy_static! {
        // mockito::Server is behind a SYNC mutex so that only one test at the same time can access it
        // otherwise, as async tests are run on multiple threads, the mock response of one test might overwrite
        // the mock response of another, causing the test to fail
        // acquiring the mutex and the beginning of each test and dropping the guard only at the end,
        // ensures that only one test at the time can set the mock response
        // this enables running the tests without specifying each time "-- --test-threads=1"
        static ref MOCK_SERVER: Arc<Mutex<mockito::Server>> =
            Arc::new(Mutex::new(mockito::Server::new_with_port(51558)));
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
        let server = &*MOCK_SERVER;
        let msg_count = 10;
        let body = CanisterOutputCertifiedMessages::mock_n(msg_count, 0).serialize();
        let path = "/ws_get_messages";
        let mut guard = server.lock().unwrap();
        // do not drop the guard until the end of this test to make sure that no other test interleaves and overwrites the mock response
        let mock = guard
            .mock("GET", path)
            .with_body(body)
            .expect(1)
            .create_async()
            .await;

        let agent = Agent::builder()
            .with_transport(ReqwestTransport::create("http://127.0.0.1:4943").unwrap())
            .build()
            .unwrap();
        let args = CanisterWsGetMessagesArguments { nonce: 0 };

        match canister_methods::ws_get_messages(&agent, &Principal::anonymous(), args.clone()).await
        {
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

        mock.assert_async().await;
        // just to make it explicit that the guard should be kept for the whole duration of the test
        drop(guard);
    }

    #[tokio::test]
    async fn should_poll_and_fail_to_validate_last_nonce() {
        let server = &*MOCK_SERVER;
        let msg_count = 10;
        let body = CanisterOutputCertifiedMessages::mock_n_with_key_error(msg_count, 0).serialize();
        let path = "/ws_get_messages";
        let mut guard = server.lock().unwrap();
        // do not drop the guard until the end of this test to make sure that no other test interleaves and overwrites the mock response
        let mock = guard
            .mock("GET", path)
            .with_body(body)
            .expect(1)
            .create_async()
            .await;

        let agent = Agent::builder()
            .with_transport(ReqwestTransport::create("http://127.0.0.1:4943").unwrap())
            .build()
            .unwrap();
        let args = CanisterWsGetMessagesArguments { nonce: 0 };

        match canister_methods::ws_get_messages(&agent, &Principal::anonymous(), args).await {
            Ok(res) => {
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

        mock.assert_async().await;
        // just to make it explicit that the guard should be kept for the whole duration of the test
        drop(guard);
    }

    #[tokio::test]
    async fn should_sleep_after_relaying() {
        let server = &*MOCK_SERVER;
        let msg_count = 10;
        let body = CanisterOutputCertifiedMessages::mock_n(msg_count, 0).serialize();
        let path = "/ws_get_messages";
        let mut guard = server.lock().unwrap();
        // do not drop the guard until the end of this test to make sure that no other test interleaves and overwrites the mock response
        let mock = guard
            .mock("GET", path)
            .with_body(body)
            .expect(1)
            .create_async()
            .await;

        let polling_interval_ms = 100;
        let (client_channel_tx, mut client_channel_rx): (
            Sender<IcWsCanisterMessage>,
            Receiver<IcWsCanisterMessage>,
        ) = mpsc::channel(100);

        let mut poller = create_poller(polling_interval_ms, client_channel_tx);
        let start_polling_instant = tokio::time::Instant::now();
        let handle = tokio::spawn(async move {
            poller.poll_and_relay().await.expect("Failed to poll");
            let end_polling_instant = tokio::time::Instant::now();
            let elapsed = end_polling_instant - start_polling_instant;
            // run 'cargo test -- --nocapture' to see the elapsed time
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
        assert_eq!(i as usize, msg_count);

        // needed to make sure that the test fails in case the task panics
        join!(handle).0.expect("task panicked");

        mock.assert_async().await;
        // just to make it explicit that the guard should be kept for the whole duration of the test
        drop(guard);
    }

    #[tokio::test]
    async fn should_not_sleep_after_relaying() {
        let server = &*MOCK_SERVER;
        let msg_count = 10;
        let body =
            CanisterOutputCertifiedMessages::mock_n_with_not_end_of_queue(msg_count, 0).serialize();
        let path = "/ws_get_messages";
        let mut guard = server.lock().unwrap();
        // do not drop the guard until the end of this test to make sure that no other test interleaves and overwrites the mock response
        let mock = guard
            .mock("GET", path)
            .with_body(body)
            .expect(1)
            .create_async()
            .await;

        let polling_interval_ms = 100;
        let (client_channel_tx, mut client_channel_rx): (
            Sender<IcWsCanisterMessage>,
            Receiver<IcWsCanisterMessage>,
        ) = mpsc::channel(100);

        let mut poller = create_poller(polling_interval_ms, client_channel_tx);
        let start_polling_instant = tokio::time::Instant::now();
        let handle = tokio::spawn(async move {
            poller.poll_and_relay().await.expect("Failed to poll");
            let end_polling_instant = tokio::time::Instant::now();
            let elapsed = end_polling_instant - start_polling_instant;
            println!("Elapsed: {:?}", elapsed);
            assert!(
                // assuming relaying a message takes at most 0.5 ms
                elapsed < Duration::from_millis((msg_count / 2) as u64)
            );
        });

        let mut i = 0;
        while let Some((msg, _)) = client_channel_rx.recv().await {
            assert_eq!(i, get_nonce_from_message(&msg.key).unwrap());
            i += 1;
        }
        assert_eq!(i as usize, msg_count);

        // needed to make sure that the test fails in case the task panics
        join!(handle).0.expect("task panicked");

        mock.assert_async().await;
        // just to make it explicit that the guard should be kept for the whole duration of the test
        drop(guard);
    }

    #[tokio::test]
    async fn should_terminate_polling_with_error() {
        let server = &*MOCK_SERVER;
        let msg_count = 15;
        let body = CanisterOutputCertifiedMessages::mock_n(msg_count, 0).serialize();
        let path = "/ws_get_messages";
        let mut guard = server.lock().unwrap();
        // do not drop the guard until the end of this test to make sure that no other test interleaves and overwrites the mock response
        let mock = guard
            .mock("GET", path)
            .with_body(body)
            .expect(2)
            .create_async()
            .await;

        let polling_interval_ms = 100;
        let (client_channel_tx, mut client_channel_rx): (
            Sender<IcWsCanisterMessage>,
            Receiver<IcWsCanisterMessage>,
        ) = mpsc::channel(100);

        let mut poller = create_poller(polling_interval_ms, client_channel_tx);
        let handle = tokio::spawn(async move { poller.run_polling().await });

        let mut i = 0;
        while let Some((msg, _)) = client_channel_rx.recv().await {
            println!(
                "Got message: {:?}",
                get_nonce_from_message(&msg.key).unwrap()
            );
            assert_eq!(i, get_nonce_from_message(&msg.key).unwrap());
            i += 1;
        }
        // when starting the second polling iteration, the poller terminates with an error
        // therefore, 'client_channel_rx' receives 'None' and 'i' should be equal to 'msg_count'
        // as the poller processed only the messages of the first polling iteration
        assert_eq!(i as usize, msg_count);

        // the poller should return an error as in the first iteration it polls the messages from 0 to 'msg_count - 1'
        // in the second iteration it polls the messages which start again from 0 (as the mock server retruns the same messages)
        // and therefore it returns an error as the poller was expecting the nonce to be equal to 'msg_count'
        assert_eq!(
            Err(format!(
                "Non consecutive nonce: expected {}, got 0",
                msg_count
            )),
            join!(handle).0.unwrap()
        );

        mock.assert_async().await;
        // just to make it explicit that the guard should be kept for the whole duration of the test
        drop(guard);
    }
}

mod test {
    use crate::canister_methods;

    #[tokio::test]
    async fn test_canister_poller() {
        let mut server = mockito::Server::new();

        let mock = server
            .mock("GET", "/ws_get_messages")
            .with_body("world")
            .create();

        let url = server.url();

        let _res = canister_methods::mock_ws_get_messages(url).await;

        mock.assert();
    }
}

use ic_cdk::api::management_canister::http_request::HttpMethod;
use reqwest::Client;
use tokio::sync::mpsc::Receiver;
use tracing::warn;

use crate::canister_methods::CanisterOutputRequest;

pub struct HttpClient {
    client: Client,
    canister_http_request_rx: Receiver<CanisterOutputRequest>,
}

impl HttpClient {
    pub fn new(canister_http_request_rx: Receiver<CanisterOutputRequest>) -> Self {
        Self {
            client: Client::new(),
            canister_http_request_rx,
        }
    }

    pub async fn start_relaying_requests(&mut self) {
        loop {
            if let Some(canister_output_request) = self.canister_http_request_rx.recv().await {
                warn!("{:?}", canister_output_request);
                match canister_output_request.method() {
                    &HttpMethod::GET => {
                        let res = self
                            .client
                            .get(canister_output_request.url())
                            .send()
                            .await
                            .unwrap();
                        warn!("{:?}", res.status());
                    },
                    &HttpMethod::POST => {
                        self.client
                            .post(canister_output_request.url())
                            .send()
                            .await
                            .unwrap();
                    },
                    &HttpMethod::HEAD => {
                        self.client
                            .head(canister_output_request.url())
                            .send()
                            .await
                            .unwrap();
                    },
                }
            }
        }
    }
}

use ic_cdk::api::management_canister::http_request::HttpMethod;
use reqwest::{Body, Client, RequestBuilder};
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
                let request = self.build_request(canister_output_request);
                let res = request.send().await.unwrap();
                warn!("{:?}", res.status());
            }
        }
    }

    fn build_request(&self, canister_output_request: CanisterOutputRequest) -> RequestBuilder {
        let mut request = match canister_output_request.method() {
            &HttpMethod::GET => self.client.get(canister_output_request.url()),
            &HttpMethod::POST => self.client.post(canister_output_request.url()),
            &HttpMethod::HEAD => self.client.head(canister_output_request.url()),
        };
        for header in canister_output_request.headers() {
            request = request.header(header.name.to_owned(), header.value.to_owned());
        }
        if let Some(body) = canister_output_request.body() {
            request = request.body(Body::from(body.to_owned()));
        }
        warn!("{:?}", request);
        request
    }
}

# IC WebSocket Gateway

[![GitHub Release](https://img.shields.io/github/v/release/omnia-network/ic-websocket-gateway)](https://github.com/omnia-network/ic-websocket-gateway/releases)
[![GitHub License](https://img.shields.io/github/license/omnia-network/ic-websocket-gateway)](https://github.com/omnia-network/ic-websocket-gateway/blob/main/LICENSE)
[![Docker Pulls](https://img.shields.io/docker/pulls/omniadevs/ic-websocket-gateway?logo=docker)](https://hub.docker.com/r/omniadevs/ic-websocket-gateway)

WebSockets enable web applications to maintain a full-duplex connection between the backend and the frontend. This allows for many different use-cases, such as notifications, dynamic content updates (e.g., showing new comments/likes on a post), collaborative editing, etc.

At the moment, the Internet Computer does not natively support WebSocket connections and developers need to resort to work-arounds in the frontend to enable a similar functionality. This results in a poor developer experience and an overload of the backend canister.

This repository contains the implementation of a WebSocket Gateway enabling clients to establish a full-duplex connection to their backend canister on the Internet Computer via the [WebSocket API](https://developer.mozilla.org/en-US/docs/Web/API/WebSockets_API).

# Running the WS Gateway

## Standalone

Make sure you have the **Rust toolchain** installed. You can find instructions [here](https://www.rust-lang.org/tools/install).

1. Run the gateway:

    In **debug** mode:

    ```bash
    cargo run
    ```

    In **release** mode:

    ```bash
    cargo build --release

    ./target/release/ic_websocket_gateway
    ```

2. The output in the terminal should look like (some lines are omitted for brevity):

    ```bash
    ...

    2024-03-14T11:19:33.649874Z  INFO ic_websocket_gateway: Gateway Agent principal: lk7eq-k74k2-khrsw-n2xau-7ujb2-f5ao7-zkpqn-cbbx2-4xn3f-aibn4-uae

    ...

    2024-03-14T11:19:33.650018Z  INFO ic_websocket_gateway::manager: Start accepting incoming connections
    ```

### Arguments available

There are some command line arguments that you can set when running the gateway:
| Argument | Description | Default |
| --- | --- | --- |
| `--gateway-address` | The **IP:port** on which the gateway will listen for incoming connections. | `0.0.0.0:8080` |
| `--ic-network-url` | The URL of the IC network to which the gateway will connect. | `http://127.0.0.1:4943` |
| `--polling-interval` | The interval (in **milliseconds**) at which the gateway will poll the canisters for new messages. | `100` |
| `--prometheus-endpoint` | The **IP:port** on which the gateway will expose Prometheus metrics. | `0.0.0.0:9090` |
| `--tls-certificate-pem-path` | The path to the TLS certificate file. See [Obtain a TLS certificate](#obtain-a-tls-certificate) for more details. | _empty_ |
| `--tls-certificate-key-pem-path` | The path to the TLS private key file. See [Obtain a TLS certificate](#obtain-a-tls-certificate) for more details. | _empty_ |
| `--opentelemetry-collector-endpoint` | OpenTelemetry collector endpoint. See [Tracing telemetry](#tracing-telemetry) for more details. | _empty_ |

## Docker

Make sure you have [Docker](https://docs.docker.com/get-docker/) installed.

A [Dockerfile](./Dockerfile) is provided. To build the image, run:

```bash
docker build -t ic-websocket-gateway .
```

Then, run the gateway with the following command (assuming you want to connect to the local dfx replica running on the default port):

```bash
docker run -p 8080:8080 ic-websocket-gateway --ic-network-url http://host.docker.internal:4943
```

A Docker image is also available at [omniadevs/ic-websocket-gateway](https://hub.docker.com/r/omniadevs/ic-websocket-gateway), that you can run with the following command:

```bash
docker run -p 8080:8080 omniadevs/ic-websocket-gateway --ic-network-url http://host.docker.internal:4943
```

> Note: if you're on an ARM machine, you have to add the `--platform` flag to the command above, since the published image is built only for the `x86_64` architecture:
>
> ```bash
> docker run --platform linux/amd64 -p 8080:8080 omniadevs/ic-websocket-gateway --ic-network-url http://host.docker.internal:4943
> ```

Have a look at the [Arguments available](#arguments-available) to configure the gateway for your needs.

## Docker Compose

Make sure you have [Docker Compose](https://docs.docker.com/compose/install/) installed.

The following compose files are available:

-   [docker-compose.yml](./docker-compose.yml)
-   [docker-compose-local.yml](./docker-compose-local.yml)
-   [docker-compose-prod.yml](./docker-compose-prod.yml)

The following sections describe how to use the different compose files to run the gateway with Docker Compose.

A visual representation of the containers in the compose files is provided in the [docker-compose-structure.png](./docs/images/docker-compose-structure.png) image.

### Local

To run the gateway in a local environment with Docker Compose, follow these steps:

1. To run all the required local structure you can execute the [start_local_docker_environment.sh](./scripts/start_local_docker_environment.sh) with the command:

    ```
     ./scripts/start_local_docker_environment.sh
    ```

    This script simply run a dfx local replica and execute the gateway with the following command:

    ```
    docker compose -f docker-compose.yml -f docker-compose-local.yml --env-file .env.local up -d --build
    ```

2. To stop and clean all the local environment, the bash script [stop_local_docker_environment.sh](./scripts/stop_local_docker_environment.sh) is provided. You can execute it with the command:

    ```
     ./scripts/stop_local_docker_environment.sh
    ```

3. The Gateway will print its principal in the container logs, just as explained in the [Standalone](#standalone) section.
4. You can verify that things are working properly by running the tests, see [Testing](#Testing)

### Production

This configuration uses the [omniadevs/ic-websocket-gateway](https://hub.docker.com/r/omniadevs/ic-websocket-gateway) image.

To run the gateway in a production environment with Docker Compose, follow these steps:

1. Set the environment variables:

    ```
    cp .env.example .env
    ```

2. To run the [docker-compose-prod.yml](./docker-compose-prod.yml) file you need a public domain (that you will put in the `DOMAIN_NAME` environment variable) and a TLS certificate for that domain (because it is configured to make the gateway run with TLS enabled). See [Obtain a TLS certificate](#obtain-a-tls-certificate) for more details.
3. Open the `443` port (or the port that you set in the `LISTEN_PORT` environment variable) on your server and make it reachable from the Internet.
4. To run all the required production containers you can execute the [start_prod_docker_environment.sh](./scripts/start_prod_docker_environment.sh) with the command:

    ```
     ./scripts/start_prod_docker_environment.sh
    ```

    This script first generates the `telemetry/prometheus/prometheus-prod.yml` config file from the `telemetry/prometheus/prometheus-template.yml` template (step required to perform the environment variables substitution) and then runs the gateway with the following command:

    ```
    docker compose -f docker-compose.yml -f docker-compose-prod.yml --env-file .env up -d
    ```

5. To stop and clean the containers, the bash script [stop_prod_docker_environment.sh](./scripts/stop_prod_docker_environment.sh) is provided. You can execute it with the command:

    ```
     ./scripts/stop_prod_docker_environment.sh
    ```

6. The Gateway will print its principal in the container logs, just as explained in the [Standalone](#standalone) section.

#### Obtain a TLS certificate

1. Buy a domain name and point it to the server where you are running the gateway.
2. Make sure the `.env` file is configured with the correct domain name, see above.
3. Obtain an SSL certificate for your domain:
    ```
    ./scripts/certbot_certonly.sh
    ```
    This will guide you in obtaining a certificate using [Certbot](https://certbot.eff.org/) in [Standalone mode](https://eff-certbot.readthedocs.io/en/stable/using.html#standalone).
    > Make sure you have port `80` open on your server and reachable from the Internet, otherwise certbot will not be able to verify your domain. Port `80` is used only for the certificate generation and can be closed afterwards.

To renew the SSL certificate, you can run the same command as above:

```
./scripts/certbot_certonly.sh
```

## Configure logging

The gateway uses the [tracing](https://docs.rs/tracing) crate for logging. There are two tracing outputs configured:

-   output to **stdout**, which has the `info` level and can be configured with the `RUST_LOG_STDOUT` env variable, see below;
-   output to a **file**, which is saved in the `data/traces/` folder and has the default `trace` level. The file name is `gateway_{start-timestamp}.log`. It can be configured with the `RUST_LOG_FILE` env variable, see below.

The `RUST_LOG` environment variable enables to set different levels for each module. See the [EnvFilter](https://docs.rs/tracing-subscriber/0.3.17/tracing_subscriber/filter/struct.EnvFilter.html) documentation for more details.
For example, to set the tracing level to `debug`, you can run:

```
RUST_LOG_FILE=ic_websocket_gateway=debug RUST_LOG_STDOUT=ic_websocket_gateway=debug cargo run
```

## Tracing telemetry

The gateway uses the [opentelemetry](https://docs.rs/opentelemetry) crate and [Grafana](https://www.grafana.com/) for tracing telemetry. To enable tracing telemetry, you have to:

-   set the `--opentelemetry-collector-endpoint` argument to point to the opentelemetry collector endpoint (leaving it empty or unset will disable tracing telemetry);
-   optionally set the `RUST_LOG_TELEMETRY` environment variable, which defaults to `trace`, following the same principles described in the [Configure logging](#configure-logging) section.

If you're deploying the gateway with Docker (see the [Docker](#docker) section), make sure you set the following varibales in the `.env` file:

**Local**:

```
OPENTELEMETRY_COLLECTOR_ENDPOINT=grpc://otlp_collector:4317
GRAFANA_TEMPO_ENDPOINT=tempo:4318
GRAFANA_TEMPO_LOCAL=true
```

**Production**:

```
OPENTELEMETRY_COLLECTOR_ENDPOINT=grpc://otlp_collector:4317
GRAFANA_TEMPO_ENDPOINT=your-grafana-cloud-tempo-endpoint
GRAFANA_TEMPO_ACCESS_TOKEN=your-grafana-cloud-tempo-basic-auth-token
GRAFANA_TEMPO_LOCAL=false
```

You can find the Tempo endpoint and create a token, by following [this](https://grafana.com/blog/2021/04/13/how-to-send-traces-to-grafana-clouds-tempo-service-with-opentelemetry-collector/) guide.

For more information about how to configure the env variables properly, checkout the [.env.example](./.env.example).

# Development

## Testing

### Unit tests

Some unit tests are provided in the [tests](./src/ic-websocket-gateway/src/tests) folder. You can run them with:

```
./scripts/unit_test.sh
```

### Integration tests

Integration tests use the IC WebSocket SDKs and are written in both Rust and Motoko. They require:

-   [Node.js](https://nodejs.org/en/download/) (version 16 or higher)
-   [dfx](https://internetcomputer.org/docs/current/developer-docs/setup/install), with which to run an [IC local replica](https://internetcomputer.org/docs/current/references/cli-reference/dfx-start/)
-   a test canister deployed on the local replica

After installing Node.js and dfx, you can run the integration tests as follows:

1. Prepare the test environment by installing dependencies and building the components by running the following command:
    ```
    ./scripts/prepare_tests.sh
    ```
2. Set the environment variables:
    ```
    cp tests/.env.example tests/.env
    ```
    When running the tests, the `tests/.env` file will be modified by dfx to add some variables.
3. Run integration tests using the Rust test canister:

    ```
    ./scripts/integration_test_rs.sh
    ```

    If you want to run tests using the Motoko test canister, run the following command instead:

    ```
    ./scripts/integration_test_mo.sh
    ```

Integration tests can be found in the [tests/src/integration](./tests/src/integration/) folder.

Tests canisters used in the integration tests can be found in the [tests/src/test_canister_rs](./tests/src/test_canister_rs/) and [tests/src/test_canister_mo](./tests/src/test_canister_mo/) folders.

### Local test script

After setting up and running the tests for the first time following the steps above, you can use the following command to run the unit and integration tests (using both the Rust and Motoko test canisters):

```
./scripts/local_test.sh
```

### Load tests

Load tests are provided in the [tests/src/load](./tests/src/load/) folder. You can run them with:

```
./scripts/run_load_test.sh
```

This script requires you to set up the test environment manually, because you usually want to keep an eye on the logs of the different components. You have to start the local replica, start the gateway and deploy the test canister. The [scripts/ci_cd_test_load.sh](./scripts/ci_cd_test_load.sh) is a good reference for how to do that.

# How it works

## Overview

![](./docs/images/high_level_view.png)

In order to enable WebSockets for a dapp running on the IC, we use a trustless intermediary - the WS Gateway - which provides a WebSocket endpoint for the frontend of the dapp, running in the user’s browser and interacts with the canister backend.

The WS Gateway is needed as a WebSocket is a one-to-one connection between client and server, but the Internet Computer does not support that due to its replicated nature. The WS Gateway relays all messages coming in via the WebSocket from the client as API canister calls for the backend and sends each message polled from the backend via the WebSocket to the corresponding client.

## Features

-   General: The WS Gateway can provide a WebSocket interface for many different dapps at the same time. Therefore, many clients can receive updates from the same or different canisters.
-   Trustless:

    -   all messages are signed: messages sent by the canister are [certified](https://internetcomputer.org/how-it-works/response-certification/); messages sent by the client signed using an [Internet Identity](https://internetcomputer.org/internet-identity) either provided by the user or generated by the [IC WebSocket Frontend SDK](https://github.com/omnia-network/ic-websocket-sdk-js). This way, the WS Gateway cannot tamper the content of the messages;
    -   all messages have a sequence number to guarantee all messages are received in the correct order;
    -   all messages are accompanied by a timestamp to prevent the WS Gateway from delaying them;
    -   all messages are acknowledged by the [IC WebSocket Backend CDK](https://github.com/omnia-network/ic-websocket-cdk-rs) so that the WS Gateway cannot block them;
    -   the [IC WebSocket Backend CDK](https://github.com/omnia-network/ic-websocket-cdk-rs) expects keep alive messages from each connected client so that it can detect if a client is not connected anymore;

-   IMPORTANT CAVEAT: NO ENCRYPTION!
    No single replica can be assumed to be trusted, so the [canister state cannot be assumed to be kept secret](https://forum.dfinity.org/t/is-persisted-data-encrypted/2156). When exchanging messages with the canister, keep in mind that in principle the messages could be seen by others on the gateway and canister side.
    This will be solved in the next version of IC WebSocket using [VetKeys](https://forum.dfinity.org/t/threshold-key-derivation-privacy-on-the-ic/16560).

## Components

1. Client:

    Client uses the [IC WebSocket Frontend SDK](https://github.com/omnia-network/ic-websocket-sdk-js) to establish the IC WebSocket connection mediated by the WS Gateway in order to communicate with the backend canister using the WebSocket API. When instantiating a new IC WebSocket connection, the client can pass an identity to the SDK in order to authenticate its messages to the canister.

    The client can instantiate a new IC WebSocket connection by calling the `IcWebSocket` constructor.

    When the client calls the `send` method of the SDK, the SDK creates a [signed envelope](https://internetcomputer.org/docs/current/references/ic-interface-spec#http-call) with the content specified by the client and signed with its identity. The envelope is sent to the WS Gateway via WebSocket and specifies the `ws_open` method of the canister.

    Once receiving a message from the WS Gateway via WebSocket, the SDK validates the messages by verifying the certificate provided using the public key of the Internet Computer.

2. WS Gateway:

    WS Gateway accepts WebSocket connections with multiple clients in order to relay their messages to and from the canister.

    Upon receiving a [signed envelope](https://internetcomputer.org/docs/current/references/ic-interface-spec#http-call) from a client, the WS Gateway relays the message to the `/canister/<canister_id>/call` endpoint of the Internet Computer. This way, the WS Gateway is transparent to the canister, which receives the request as if sent directly by the client which signed it.

    In order to get updates from the canister, the WS Gateway polls the caniser by sending periodic queries to the `ws_get_messages` method. Upon receiving a response to a query, the WS Gateway relays the contained message and certificate to the corresponding client using the WebSocket.

3. Backend canister:

    The backend canister uses the [IC WebSocket Backend CDK](https://github.com/omnia-network/ic-websocket-cdk-rs) which exposes the methods of a typical WebSocket server (`ws_open`, `ws_message`, `ws_error`, `ws_close`) plus the `ws_get_messages` method polled by the WS Gateway. The backend canister must specify the callback functions which should be executed on different WebSocket events (`open`, `message`, `error`, `close`). The CDK triggers the corresponding callback upon receiving a request on one of the WebSocket methods.

    The CDK also implements the logic necessary to detect a possible WS Gateway misbehaviour.

    In order to send an update to one of its clients, the canister calls the `ws_send` method of the CDK specifying the message that it wants to be delivered to the client. The CDK pushes this message in a FIFO queue together with all the other clients' messages. Upon receiving a query call to the `ws_get_messages` method, the CDK returns the messages in this queue (up to a certain limit), together with a certificate which proves to the clients that the messages are actually the ones sent from the canister even if relayed by the WS Gateway.

## Message Flow

For more information on the messages exchanged between client, WS Gateway and canister, checkout the [IC WebSocket Protocol](./docs/ic-ws-protocol.md).

# License

MIT License. See [LICENSE](./LICENSE).

# Contributing

Feel free to open issues, pull requests, join our [Discord](https://discord.com/invite/pkPY4AqT) and reach out to us!

## Massimo Albarello

-   [Linkedin](https://linkedin.com/in/massimoalbarello)
-   [Twitter](https://twitter.com/MaxAlbarello)
-   [Calendly](https://cal.com/massimoalbarello/meeting)
-   [Email](mailto:mez@omnia-network.com)

## Luca Bertelli

-   [Linkedin](https://www.linkedin.com/in/luca-bertelli-407041128/)
-   [Twitter](https://twitter.com/ilbert_luca)
-   [Calendly](https://cal.com/lucabertelli/ic-websocket)
-   [Email](mailto:liuc@omnia-network.com)

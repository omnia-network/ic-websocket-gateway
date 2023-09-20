# IC WebSocket Gateway

WebSockets enable web applications to maintain a full-duplex connection between the backend and the frontend. This allows for many different use-cases, such as notifications, dynamic content updates (e.g., showing new comments/likes on a post), collaborative editing, etc.

At the moment, WebSockets are not supported for dapps on the Internet Computer and developers need to resort to work-arounds in the frontend to enable a similar functionality. This results in a poor developer experience and an overload of the backend canister.

This repository contains the implementation of a WebSocket Gateway enabling clients to establish a full-duplex connection to their backend canister on the Internet Computer via the WebSocket API.

# Running the WS Gateway

## Prerequisites

Make sure you have the **Rust toolchain** installed. You can find instructions [here](https://www.rust-lang.org/tools/install).

## Standalone

1. Run the gateway:
    ```
    cargo run
    ```
2. After the gateway starts, it prints something like:

    ```
    2023-08-01T07:55:58.315793Z INFO ic_websocket_gateway::gateway_server: Gateway Agent principal: sqdfl-mr4km-2hfjy-gajqo-xqvh7-hf4mf-nra4i-3it6l-neaw4-soolw-tae
    ```

    This is the principal that the gateway uses to interact with the canister IC WebSocket CDK.

3. Copy the gateway principal as you will need it to initialize the canister CDK, so that the canister can authenticate the gateway. How to do this is explained in the [ic_websocket_example](https://github.com/omnia-network/ic_websocket_example#running-the-project-locally) README.

### Options available

There are some command line arguments that you can set when running the gateway:
| Argument | Description | Default |
| --- | --- | --- |
| `--gateway-address` | The **IP:port** on which the gateway will listen for incoming connections. | `0.0.0.0:8080` |
| `--subnet-url` | The URL of the IC subnet to which the gateway will connect. | `http://127.0.0.1:4943` |
| `--polling-interval` | The interval (in **milliseconds**) at which the gateway will poll the canisters for new messages. | `100` |
| `--tls-certificate-pem-path` | The path to the TLS certificate file. See [Obtain a TLS certificate](#obtain-a-tls-certificate) for more details. | _empty_ |
| `--tls-certificate-key-pem-path` | The path to the TLS private key file. See [Obtain a TLS certificate](#obtain-a-tls-certificate) for more details. | _empty_ |

## Docker

A [Dockerfile](./Dockerfile) is provided, together with a [docker-compose.yml](./docker-compose.yml) file to run the gateway. Make sure you have [Docker](https://docs.docker.com/get-docker/) and [Docker Compose](https://docs.docker.com/compose/install/) installed.

1. Set the environment variables:

    ```
    cp .env.example .env
    ```
2. The docker-compose.yml file is configured to make the gateway run with TLS enabled. For this, you need a public domain (that you will put in the `DOMAIN_NAME` environment variable) and a TLS certificate for that domain. See [Obtain a TLS certificate](#obtain-a-tls-certificate) for more details.
3. Open the `443` port (or the port that you set in the `LISTEN_PORT` environment variable) on your server and make it reachable from the Internet.
4. Run the gateway:
    ```
    docker compose up
    ```
5. The Gateway will print its principal in the container logs, just as explained above.
6. Whenever you want to rebuild the gateway image, run:
    ```
    docker compose up --build
    ```

### Obtain a TLS certificate

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
- output to **stdout**, which has the `info` level and cannot be changed;
- output to a **file**, which is saved in the `data/traces/` folder and has the default `info` level. The file name is `gateway_{start-timestamp}.log`.
    It's possible to configure the file tracing level by setting the `RUST_LOG` environment variable. For example, to set the tracing level to `debug`, you can run:
    ```
    RUST_LOG=ic_websocket_gateway=debug cargo run
    ```
    The `RUST_LOG` environment variable enables to set different levels for each module. See the [EnvFilter](https://docs.rs/tracing-subscriber/0.3.17/tracing_subscriber/filter/struct.EnvFilter.html) documentation for more details.

# Development

## Testing

### Unit tests

Some unit tests are provided in the [unit_tests.rs](./src/ic-websocket-gateway/src/unit_tests.rs) file. You can run them with:

```
cargo test -- --test-threads=1
```

### Integration tests
Integration tests require:
- [Node.js](https://nodejs.org/en/download/) (version 16 or higher)
- [dfx](https://internetcomputer.org/docs/current/developer-docs/setup/install), with which to run an [IC local replica](https://internetcomputer.org/docs/current/references/cli-reference/dfx-start/) 
- a test canister deployed on the local replica

After installing Node.js and dfx, you can run the integration tests as follows:

1. Run a local replica:

    ```
    dfx start --clean --background
    ```

2. Run the gateway:
    ```
    cargo run
    ```
3. Copy the principal printed on the terminal (as explained above)
4. Open a new terminal and install test dependencies:
    ```
    ./scripts/install_test_dependencies.sh
    ```
5. Move to the directory `tests/test_canister` and deploy a test canister using the IC WebSocket CDK:
    ```
    dfx deploy test_canister --argument '(opt "<gateway-principal-obtained-in-step-3>")'
    ```
6. Move to the directory `tests/integration` and set the environment variables:
    ```
    cp .env.example .env
    ```
7. Run the test:
    ```
    npm test
    ```

### Local test script

To make it easier to run both the unit and integration tests, a script is provided that runs the local replica, the gateway and the tests in sequence. You can run it with:

```
./scripts/local_test.sh
```

# How it works

TODO: update this section

## Overview

![](./docs/images/image2.png)

In order to enable WebSockets for a dapp running on the IC, we use an intermediary, called WS Gateway, that provides a WebSocket endpoint for the frontend of the dapp, running in the user’s browser and interfaces with the canister backend.

The gateway is needed as a WebSocket is a one-to-one connection between client and server, but the Internet Computer does not support that due to its replicated nature. The gateway translates all messages coming in on the WebSocket from the client to API canister calls for the backend and sends all messages coming from the backend on the Internet Computer out on the WebSocket with the corresponding client.

## Features

-   General: The WS Gateway can provide WebSockets for many different dapps at the same time. A frontend can connect through any gateway to the backend (e.g., through the geographically closest one to reduce latency).
-   Trustless: In order to make it impossible for the gateway to tamper with messages:

    -   all messages are signed: messages sent by the canister are [certified](https://internetcomputer.org/how-it-works/response-certification/); messages sent by the client signed by it;
    -   all messages have a sequence number to guarantee all messages are received in the correct order;
    -   all messages are accompanied by a timestamp.

-   IMPORTANT CAVEAT: NO ENCRYPTION!
    No single replica can be assumed to be trusted, so the canister state cannot be assumed to be kept secret. This means that when exchanging messages with the canister, we have to keep in mind that in principle the messages could be seen by others on the canister side.
    We could encrypt the messages between the client and the canister (so that they’re hidden from the gateway and any other party that cannot see the canister state), but we chose not to do so to make it clear that **in principle the messages could be seen by others on the canister side**.

## Components

1. Client:

    Client uses the [IC WebSocket SDK](https://github.com/omnia-network/ic-websocket-sdk-js) to establish the IC WebSocket connection and communicate with its backend canister via the WebSocket API. Client will sign its messages.

    - Generates a public/private ed25519 key pair.
    - Makes an update call to the canister to register the public key. Canister remembers the caller associated with this key. The call returns client_id.
    - Opens a WebSocket to the given gateway address.
    - Sends the first message with its client_id and the canister it wants to connect to. The message is signed with the private key.
    - Receives certified canister messages from the WebSocket.
    - Sends messages to the canister via the WebSocket Gateway. Messages are signed with the private key.

2. WS Gateway:

    WS Gateway accepts WebSocket connections to enable clients to communicate with canisters with WebSockets. Gateway can only pass on messages between clients and canisters and cannot forge messages.

    - Accepts WebSocket connections.
    - Expects the first message from the WebSocket to contain canister_id and client_id, signed.
    - Makes an update call ws_open to the canister with the given id passing on the message. The method returns true if the canister correctly verifies the signature with the previously registered client_id. If the method returns false, the WebSocket is dropped.
    - If ws_open returns true, the gateway spawns a polling task that makes query calls to ws_get_messages.
    - ws_get_messages returns certified messages from the canister to the clients that opened the WebSocket with this gateway. The gateway sends respective messages to the clients over the WebSockets.
    - After receiving messages, the polling task increases the message nonce to receive later messages.
    - Forwards signed client messages received over the WebSocket to the canister with ws_message.
    - The gateway calls ws_close when the WebSocket with the client closes for any reason.

3. Backend canister:

    The backend canister uses the [IC WebSocket CDK](https://github.com/omnia-network/ic-websocket-cdk-rs) which makes it possible for the WS Gateway to facilitate WebSocket connections with clients.

    - Receives client public keys. Records the caller associated with the given public key.
    - Receives calls to ws_open. Verifies that the provided signature corresponds to the given client_id. Records the caller as the gateway that will poll for messages.
    - Receives client messages to ws_message. Verifies that the provided signature corresponds to the recorded client_id.
    - Queues outgoing messages in queues corresponding to the recorded gateways. Puts the associated hashes in ic_certified_map to produce certificates.
    - When ws_close is called by the gateway corresponding to the provided client_id, the client info is deleted.

## Message flow

![](./docs/images/image1.png)

1. Client generates an ed25519 key pair and makes an update call to the canister to register the public key. Canister remembers the caller associated with this key. The call returns client_id.
2. Client opens a WebSocket with the gateway.
3. Client sends the first message with its client_id and the canister_id it wants to connect to. The message is signed with the private key.
4. The gateway makes an update call ws_open to the canister with the given id passing on the message. The method returns true if the canister correctly verifies the signature with the previously registered client_id.
5. Client composes a message and signs it. Client sends the message to the gateway over the WebSocket. The gateway makes an update call to forward the message to the canister.

    In the other direction, the canister composes a message and places its hash in the certified data structure. The gateway polls for messages and retrieves the message together with the certificate. The gateway passes on the message and the certificate to the client over the WebSocket.

6. Whenever the WebSocket with the client is closed, the gateway calls ws_close. Afterwards no more messages can be sent from the canister to the client.

# License

TODO: add a license

# Contributing

Feel free to open issues and pull requests.

# Introduction

WebSockets enable web applications to maintain a full-duplex connection between the backend and the frontend. This allows for many different use-cases, such as notifications, dynamic content updates (e.g., showing new comments/likes on a post), collaborative editing, etc.

At the moment, WebSockets are not supported for dapps on the Internet Computer and developers need to resort to work-arounds in the frontend to enable a similar functionality. This results in a poor developer experience and an overload of the backend canister.

This repository contains the implementation of a WebSocket Gateway enabling clients to establish a full-duplex connection to their backend canister on the Internet Computer via the WebSocket API.

# Running the demo locally

1. Run a local replica: `dfx start --clean --background`
2. Run the gateway: `cargo run` and copy its principal which gets printed on the terminal
3. Open a new terminal and install test dependencies: `./scripts/install_test_dependencies.sh`
4. Open the file `tests/test_canister/src/canister.rs` paste the gateway principal as a value of `GATEWAY_PRINCIPAL`
5. Move to the directory `tests/test_canister` and deploy a test canister using the IC WebSocket CDK: `dfx deploy`
6. Move to the directory `tests/integration` and set the environment variables: `cp .env.example .env`, then run the test: `npm test`

# Overview

![](./docs/images/image2.png)

In order to enable WebSockets for a dapp running on the IC, we use an intermediary, called WS Gateway, that provides a WebSocket endpoint for the frontend of the dapp, running in the user’s browser and interfaces with the canister backend.

The gateway is needed as a WebSocket is a one-to-one connection between client and server, but the Internet Computer does not support that due to its replicated nature. The gateway translates all messages coming in on the WebSocket from the client to API canister calls for the backend and sends all messages coming from the backend on the Internet Computer out on the WebSocket with the corresponding client.

# Features

-   General: The WS Gateway can provide WebSockets for many different dapps at the same time. A frontend can connect through any gateway to the backend (e.g., through the geographically closest one to reduce latency).
-   Trustless: In order to make it impossible for the gateway to tamper with messages:

    -   all messages are signed: messages sent by the canister are [certified](https://internetcomputer.org/how-it-works/response-certification/); messages sent by the client signed by it;
    -   all messages have a sequence number to guarantee all messages are received in the correct order;
    -   all messages are accompanied by a timestamp.

-   IMPORTANT CAVEAT: NO ENCRYPTION!
    No single replica can be assumed to be trusted, so the canister state cannot be assumed to be kept secret. This means that when exchanging messages with the canister, we have to keep in mind that in principle the messages could be seen by others on the canister side.
    We could encrypt the messages between the client and the canister (so that they’re hidden from the gateway and any other party that cannot see the canister state), but we chose not to do so to make it clear that **in principle the messages could be seen by others on the canister side**.

# Components

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

# Message flow

![](./docs/images/image1.png)

1. Client generates an ed25519 key pair and makes an update call to the canister to register the public key. Canister remembers the caller associated with this key. The call returns client_id.
2. Client opens a WebSocket with the gateway.
3. Client sends the first message with its client_id and the canister_id it wants to connect to. The message is signed with the private key.
4. The gateway makes an update call ws_open to the canister with the given id passing on the message. The method returns true if the canister correctly verifies the signature with the previously registered client_id.
5. Client composes a message and signs it. Client sends the message to the gateway over the WebSocket. The gateway makes an update call to forward the message to the canister.

    In the other direction, the canister composes a message and places its hash in the certified data structure. The gateway polls for messages and retrieves the message together with the certificate. The gateway passes on the message and the certificate to the client over the WebSocket.

6. Whenever the WebSocket with the client is closed, the gateway calls ws_close. Afterwards no more messages can be sent from the canister to the client.

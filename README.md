# Introduction

WebSockets enable web applications to maintain a full-duplex connection between the backend and the frontend. This allows for many different use-cases, such as notifications, dynamic content updates (e.g., showing new comments/likes on a post), collaborative editing, etc.

At the moment, WebSockets are not supported for dapps on the Internet Computer and developers need to resort to work-arounds in the frontend to enable a similar functionality.

This repository contains a proof-of-concept implementation for WebSockets on the Internet Computer. This proof-of-concept uses an intermediary service that provides a WebSocket endpoint to the dapp frontend and interfaces with the canister backend hosted on the IC.

In the following, we provide an overview of the architecture and the instructions to run it yourself.

# Running the demo locally

1. Run a local replica: `dfx start`
2. Run the gateway: navigate to ic_websocket_gateway and `cargo run`
3. Deploy the canisters to the local replica:
    - navigate to ic_websocket_canisters,
    - `npm install`,
    - `dfx deploy`,
    - put the correct backend canister id in index.js of the frontend canister,
    - `dfx deploy`.
4. Look at the frontend canister with the link given by dfx.

# Contributing

This repository accepts external contributions if you accept the Contributor Lincense Agreement: https://github.com/dfinity/cla/.

# Overview

![](./docs/images/image2.png)

In order to enable WebSockets for a dapp running on the IC, we use an intermediary, which we call gateway, that provides a WebSocket endpoint for the frontend of the dapp, running in the user’s browser and interfaces with the canister backend.

The gateway is needed as a WebSocket is a one-to-one connection between client and server, but the Internet Computer does not support that due to its replicated nature. The gateway translates all messages coming in on the WebSocket from the client to API canister calls for the backend and sends all messages coming from the backend on the Internet Computer out on the WebSocket with the corresponding client.

# Features

* General: A gateway can provide WebSockets for many different dapps at the same time. A frontend can connect through any gateway to the backend (e.g., through the geographically closest one to reduce latency).
* Trustless: In order to make it impossible for the gateway to tamper with messages:
  - all messages are signed: messages sent by the canister are [certified](https://internetcomputer.org/how-it-works/response-certification/); messages sent by the client signed by it;
  - all messages have a sequence number to guarantee all messages are received in the correct order;
  - all messages are accompanied by a timestamp.

* IMPORTANT CAVEAT: NO ENCRYPTION!
No single replica can be assumed to be trusted, so the canister state cannot be assumed to be kept secret. This means that when exchanging messages with the canister, we have to keep in mind that in principle the messages could be seen by others on the canister side.
We could encrypt the messages between the client and the canister (so that they’re hidden from the gateway and any other party that cannot see the canister state), but we chose not to do so to make it clear that **in principle the messages could be seen by others on the canister side**.

# Components

1. Client:

   Client is the user that opens the websocket to communicate with a canister. Client will sign its messages.
   - Generates a public/private ed25519 key pair.
   - Makes an update call to the canister to register the public key. Canister remembers the caller associated with this key. The call returns client_id.
   - Opens a websocket to the given gateway address.
   - Sends the first message with its client_id and the canister it wants to connect to. The message is signed with the private key.
   - Receives certified canister messages from the websocket.
   - Sends messages to the canister to the websocket. Messages are signed with the private key.

2. Gateway:
   
   Gateway accepts websocket connections to enable clients to communicate with canisters with websockets. Gateway can only pass on messages between clients and canisters and cannot forge messages.
   - Accepts websocket connections.
   - Expects the first message from the websocket to contain canister_id and client_id, signed.
   - Makes an update call ws_open to the canister with the given id passing on the message. The method returns true if the canister correctly verifies the signature with the previously registered client_id. If the method returns false, the websocket is dropped.
   - If ws_open returns true, the gateway spawns a polling task that makes query calls to ws_get_messages.
   - ws_get_messages returns certified messages from the canister to the clients that opened the websocket with this gateway. The gateway sends respective messages to the clients over the websockets.
   - After receiving messages, the polling task increases the message nonce to receive later messages.
   - Forwards signed client messages received over the websocket to the canister with ws_message.
   - The gateway calls ws_close when the websocket with the client closes for any reason.

3. Backend canister:
   
   The backend canister exposes an interface that makes it possible for the gateway to facilitate websocket connections with clients.
   - Receives client public keys. Records the caller associated with the given public key.
   - Receives calls to ws_open. Verifies that the provided signature corresponds to the given client_id. Records the caller as the gateway that will poll for messages.
   - Receives client messages to ws_message. Verifies that the provided signature corresponds to the recorded client_id.
   - Queues outgoing messages in queues corresponding to the recorded gateways. Puts the associated hashes in ic_certified_map to produce certificates.
   - Upon queuing outgoing messages, the canister deletes up to two past messages from the queues and the corresponding hashes from the certified map if the messages were sent at least five minutes prior.
   - When ws_close is called by the gateway corresponding to the provided client_id, the client info is deleted.

# Message flow

![](./docs/images/image1.png)

1. Client generates an ed25519 key pair and makes an update call to the canister to register the public key. Canister remembers the caller associated with this key. The call returns client_id.
2. Client opens a websocket with the gateway.
3. Client sends the first message with its client_id and the canister_id it wants to connect to. The message is signed with the private key.
4. The gateway makes an update call ws_open to the canister with the given id passing on the message. The method returns true if the canister correctly verifies the signature with the previously registered client_id.
5. Client composes a message and signs it. Client sends the message to the gateway over the websocket. The gateway makes an update call to forward the message to the canister.
  
   In the other direction, the canister composes a message and places its hash in the certified data structure. The gateway polls for messages and retrieves the message together with the certificate. The gateway passes on the message and the certificate to the client over the websocket.
6. Whenever the websocket with the client is closed, the gateway calls ws_close. Afterwards no more messages can be sent from the canister to the client.

# Backend canister interface

* **"ws_register": (blob) -> (nat64);**

  Client submits its public key in binary before opening the websocket. Method returns client_id.
* **"ws_get_client_key": (nat64) -> (blob);**

  Gateway calls this method to get a client’s public key, in order to verify its signature and accept the client’s websocket connection as valid.
* **"ws_open": (blob, blob) -> (bool);**

  Gateway calls this method to register to poll for client’s messages. First argument is the cbor encoding of
  ```
  {
    client_id: u64,
    canister_id: String,
  }
  ```
  The second argument is the signature of the first argument corresponding to the client_id.
* **"ws_close": (nat64) -> ();**

  The gateway calls this method to close the websocket corresponding to the given client_id. The canister deletes the clients data and afterwards cannot queue any more messages for the client.
* **"ws_get_messages": (nat64) -> (CertMessages) query;**

  The canister returns the messages with the following fields:
  - `client_id: u64`
    Client will check the message is indeed for them.
  - `sequence_num: u64`
    Client will check all messages are forwarded, and that the order is preserved.
  - `timestamp: u64`
    Timestamp at which the message was published. Can be used by the client to see the delay with which the messages are forwarded.
  - `message: Vec<u8>`
    The message contents encoded in binary form.
  The message is cbor encoded and provided as val in the candid type:
  ```
  type Message = record {
    client_id: nat64;
    key: text;
    val: blob;
  };
  ```
  The field ‘key’ provides the argument under which the hash of ‘val’ is stored in the certified map.
  
  Up to 50 messages queued for clients of the calling gateway are returned as the candid type:
  ```
  type CertMessages = record {
    messages: vec Message;
    cert: blob;
    tree: blob;
  };
  ```
  The messages are stored in the certified map under consecutive keys. The provided ‘tree’ includes all keys in the relevant range, and thus the fields ‘cert’ and ‘tree’ serve as the certificate for all clients to which messages are addressed.
* **"ws_message": (blob) -> (bool);**

  Gateway calls this method to pass a message from the client to the canister. The argument is the cbor encoding of the candid type
  ```
  record {
    val: blob;
    sig: blob;
  };
  ```
  where ‘val’ is the cbor encoding of a message with the abovementioned fields:
  ```
  client_id: u64
  sequence_num: u64
  timestamp: u64
  message: Vec<u8>
  ```
  and ‘sig’ is the signature corresponding to the client.

# Issues and future work

1. The provided websocket server example is very rudimentary and needs to improved for real use, e.g. use SSL, harden against DDoS attacks, port scanning, proper firewall rules. The server can panic if used incorrectly, e.g. if client requests to connect to wrong canister id. Some data might be left over and not properly cleaned up after closing connections, e.g. in the current state after all connections to a certain canister are closed, the gateway continues polling for messages.
2. Error handling and reliability need to be improved. E.g. the canister panics instead of returning informative error messages if incorrect arguments are passed or methods are called in incorrect order.
3. Heartbeat messages are not implemented yet.
Heartbeat messages would ensure that the client/canister can detect the gateway crashing or misbehaving by delaying messages, and timeout. As of yet, if the gateway crashes or misbehaves, it may appear to the canister that the connection is still open, while the websocket between the gateway and the client has been closed (and vice versa).
4. The authentication of the identity used to register the websocket might expire (for example if using the Internet Identity), but the resulting websocket connections don't expire, constituting a security risk.

/**
 * @jest-environment node
 */

import { Actor, Cbor } from "@dfinity/agent";
import { Principal } from "@dfinity/principal";
import {
  canisterId,
  client1,
  client2,
  commonAgent,
  gateway1,
  gateway1Data,
  gateway2,
} from "./utils/actors";
import { getKeyPair, getSignedMessage } from "./utils/crypto";
import {
  WebsocketMessage,
  getCertifiedMessageKey,
  getWebsocketMessage,
  isMessageBodyValid,
  isValidCertificate,
  wsClose,
  wsMessage,
  wsOpen,
  wsRegister,
  wsSend,
  wsWipe,
} from "./utils/api";
import {
  CanisterOutputCertifiedMessages,
  CanisterWsCloseResult,
  CanisterWsGetMessagesResult,
  CanisterWsMessageResult,
  CanisterWsOpenResult,
  CanisterWsRegisterResult,
  CanisterWsSendResult,
} from "../../src/declarations/ic_websocket_backend/ic_websocket_backend.did";
// import { TestContext } from "lightic";

const MAX_NUMBER_OF_RETURNED_MESSAGES = 10; // set in the CDK
const SEND_MESSAGES_COUNT = MAX_NUMBER_OF_RETURNED_MESSAGES + 2; // test with more messages to check the indexes and limits

// const context = new TestContext();
let client1KeyPair: { publicKey: Uint8Array; secretKey: Uint8Array | string; };
let client2KeyPair: { publicKey: Uint8Array; secretKey: Uint8Array | string; };

const assignKeyPairsToClients = async () => {
  if (!client1KeyPair) {
    client1KeyPair = await getKeyPair();
  }
  if (!client2KeyPair) {
    client2KeyPair = await getKeyPair();
  }
};

// testing again canister takes quite a while
jest.setTimeout(60_000);

describe("Canister - ws_register", () => {
  beforeAll(async () => {
    await assignKeyPairsToClients();
  });

  afterAll(async () => {
    await wsWipe(gateway1);
  });

  it("should register a client", async () => {
    const res = await wsRegister({
      clientActor: client1,
      clientKey: client1KeyPair.publicKey,
    });

    expect(res).toMatchObject<CanisterWsRegisterResult>({
      Ok: null,
    });
  });
});

describe("Canister - ws_open", () => {
  beforeAll(async () => {
    await assignKeyPairsToClients();

    await wsRegister({
      clientActor: client1,
      clientKey: client1KeyPair.publicKey,
    }, true);
  });

  afterAll(async () => {
    await wsWipe(gateway1);
  });

  it("fails for a gateway which is not registered", async () => {
    const res = await wsOpen({
      clientPublicKey: client1KeyPair.publicKey,
      clientSecretKey: client1KeyPair.secretKey,
      canisterId,
      gatewayActor: gateway2,
    });

    expect(res).toMatchObject<CanisterWsOpenResult>({
      Err: "caller is not the gateway that has been registered during CDK initialization",
    });
  });

  it("fails for a client which is not registered", async () => {
    const res = await wsOpen({
      clientPublicKey: client2KeyPair.publicKey,
      clientSecretKey: client2KeyPair.secretKey,
      canisterId,
      gatewayActor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsOpenResult>({
      Err: "client's public key has not been previously registered by client",
    });
  });

  it("fails for an invalid signature", async () => {
    // sign message with client2 secret key but send client1 public key
    const res = await wsOpen({
      clientPublicKey: client1KeyPair.publicKey,
      clientSecretKey: client2KeyPair.secretKey,
      canisterId,
      gatewayActor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsOpenResult>({
      Err: "Signature doesn't verify",
    });
  });

  it("should open the websocket for a registered client", async () => {
    const res = await wsOpen({
      clientPublicKey: client1KeyPair.publicKey,
      clientSecretKey: client1KeyPair.secretKey,
      canisterId,
      gatewayActor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsOpenResult>({
      Ok: {
        client_key: client1KeyPair.publicKey,
        canister_id: Principal.fromText(canisterId),
        nonce: BigInt(0),
      },
    });
  });
});

describe("Canister - ws_message", () => {
  beforeAll(async () => {
    await assignKeyPairsToClients();

    await wsRegister({
      clientActor: client1,
      clientKey: client1KeyPair.publicKey,
    }, true);

    await wsOpen({
      clientPublicKey: client1KeyPair.publicKey,
      clientSecretKey: client1KeyPair.secretKey,
      canisterId,
      gatewayActor: gateway1,
    }, true);
  });

  afterAll(async () => {
    await wsWipe(gateway1);
  });

  it("fails if a non registered gateway sends an IcWebSocketEstablished message", async () => {
    const res = await wsMessage({
      message: {
        IcWebSocketEstablished: client1KeyPair.publicKey,
      },
      actor: gateway2,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "caller is not the gateway that has been registered during CDK initialization",
    });
  });

  it("fails if a non registered gateway sends a RelayedByGateway message", async () => {
    const content = getWebsocketMessage(client1KeyPair.publicKey, 0);
    const res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway2,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "caller is not the gateway that has been registered during CDK initialization",
    });
  });

  it("fails if a non registered client sends a DirectlyFromClient message", async () => {
    const content = Cbor.encode({ text: "pong" });
    const res = await wsMessage({
      message: {
        DirectlyFromClient: {
          client_key: client2KeyPair.publicKey,
          message: new Uint8Array(content),
        }
      },
      actor: client2,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "client was was not authenticated with II when it registered its public key",
    });
  });

  it("fails if a non registered client sends a DirectlyFromClient message using a registered client key", async () => {
    const content = Cbor.encode({ text: "pong" });
    const res = await wsMessage({
      message: {
        DirectlyFromClient: {
          client_key: client1KeyPair.publicKey,
          message: new Uint8Array(content),
        }
      },
      actor: client2,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "caller is not the same that registered the public key",
    });
  });

  it("fails if a registered gateway sends an IcWebSocketEstablished message for a non registered client", async () => {
    const res = await wsMessage({
      message: {
        IcWebSocketEstablished: client2KeyPair.publicKey,
      },
      actor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "client's public key has not been previously registered by client",
    });
  });

  it("fails if a registered gateway sends a RelayedByGateway message with an invalid signature", async () => {
    const content = getWebsocketMessage(client1KeyPair.publicKey, 0);
    const res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client2KeyPair.secretKey),
      },
      actor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "Signature doesn't verify",
    });
  });

  it("fails if a registered gateway sends a wrong RelayedByGateway message", async () => {
    // empty message
    let content = Cbor.encode({});
    let res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway1,
    });
    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "missing field `client_key`",
    });

    // with client_key
    content = Cbor.encode({
      client_key: client1KeyPair.publicKey,
    });
    res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway1,
    });
    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "missing field `sequence_num`",
    });

    // with client_key, sequence_num
    content = Cbor.encode({
      client_key: client1KeyPair.publicKey,
      sequence_num: 0,
    });
    res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway1,
    });
    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "missing field `timestamp`",
    });

    // with client_key, sequence_num, timestamp
    content = Cbor.encode({
      client_key: client1KeyPair.publicKey,
      sequence_num: 0,
      timestamp: Date.now(),
    });
    res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway1,
    });
    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "missing field `message`",
    });

    // wrong message type
    content = Cbor.encode({
      client_key: client1KeyPair.publicKey,
      sequence_num: 0,
      timestamp: Date.now(),
      message: "test",
    });
    const throwingCall = wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway1,
    });
    await expect(throwingCall).rejects.toThrow();
  });

  it("fails if registered gateway sends a DirectlyFromClient message", async () => {
    const content = Cbor.encode({ text: "pong" });
    const res = await wsMessage({
      message: {
        DirectlyFromClient: {
          client_key: client1KeyPair.publicKey,
          message: new Uint8Array(content),
        }
      },
      actor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "caller is not the same that registered the public key",
    });
  });

  it("fails if registered gateway sends a RelayedByGateway message with a wrong sequence number", async () => {
    const appMessage = Cbor.encode({ text: "pong" });
    let content = getWebsocketMessage(client1KeyPair.publicKey, 1, appMessage);
    let res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway1,
    });
    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "incoming client's message relayed from WS Gateway does not have the expected sequence number",
    });

    // send a correct message to increase the sequence number
    content = getWebsocketMessage(client1KeyPair.publicKey, 0, appMessage);
    res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway1,
    });
    expect(res).toMatchObject<CanisterWsMessageResult>({
      Ok: null,
    });

    // send a message with the old sequence number
    content = getWebsocketMessage(client1KeyPair.publicKey, 0, appMessage);
    res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway1,
    });
    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "incoming client's message relayed from WS Gateway does not have the expected sequence number",
    });

    // send a message with a sequence number that is too high
    content = getWebsocketMessage(client1KeyPair.publicKey, 2, appMessage);
    res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway1,
    });
    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "incoming client's message relayed from WS Gateway does not have the expected sequence number",
    });
  });

  it("fails if a registered gateway sends a RelayedByGateway for a registered client that doesn't have open connection", async () => {
    // register another client, but don't call ws_open for it
    await wsRegister({
      clientActor: client2,
      clientKey: client2KeyPair.publicKey,
    }, true);

    const content = getWebsocketMessage(client2KeyPair.publicKey, 0);
    const res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client2KeyPair.secretKey),
      },
      actor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Err: "expected incoming message num not initialized for client",
    });
  });

  it("a registered gateway should send a message (IcWebSocketEstablished) for a registered client", async () => {
    const res = await wsMessage({
      message: {
        IcWebSocketEstablished: client1KeyPair.publicKey,
      },
      actor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Ok: null,
    });
  });

  it("a registered gateway should send a message (RelayedByGateway) for a registered client", async () => {
    const appMessage = Cbor.encode({ text: "pong" });
    // the message with sequence number 0 has been sent in a previous test, so we send a message with sequence number 1
    const content = getWebsocketMessage(client1KeyPair.publicKey, 1, appMessage);
    const res = await wsMessage({
      message: {
        RelayedByGateway: await getSignedMessage(content, client1KeyPair.secretKey),
      },
      actor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Ok: null,
    });
  });

  it("a registered client should send a message (DirectlyFromClient)", async () => {
    const appMessage = Cbor.encode({ text: "pong" });
    const res = await wsMessage({
      message: {
        DirectlyFromClient: {
          client_key: client1KeyPair.publicKey,
          message: new Uint8Array(appMessage),
        }
      },
      actor: client1,
    });

    expect(res).toMatchObject<CanisterWsMessageResult>({
      Ok: null,
    });
  });
});

describe("Canister - ws_get_messages (failures,empty)", () => {
  beforeAll(async () => {
    await assignKeyPairsToClients();

    await wsRegister({
      clientActor: client1,
      clientKey: client1KeyPair.publicKey,
    }, true);

    await wsOpen({
      clientPublicKey: client1KeyPair.publicKey,
      clientSecretKey: client1KeyPair.secretKey,
      canisterId,
      gatewayActor: gateway1,
    }, true);

    await commonAgent.fetchRootKey();
  });

  afterAll(async () => {
    await wsWipe(gateway1);
  });

  it("fails if a non registered gateway tries to get messages", async () => {
    const res = await gateway2.ws_get_messages({
      nonce: BigInt(0),
    });

    expect(res).toMatchObject<CanisterWsGetMessagesResult>({
      Err: "caller is not the gateway that has been registered during CDK initialization",
    });
  });

  it("registered gateway should receive empty messages if no messages are available", async () => {
    let res = await gateway1.ws_get_messages({
      nonce: BigInt(0),
    });

    expect(res).toMatchObject<CanisterWsGetMessagesResult>({
      Ok: {
        messages: [],
        cert: new Uint8Array(),
        tree: new Uint8Array(),
      },
    });

    res = await gateway1.ws_get_messages({
      nonce: BigInt(100), // high nonce to make sure the indexes are calculated correctly in the canister
    });

    expect(res).toMatchObject<CanisterWsGetMessagesResult>({
      Ok: {
        messages: [],
        cert: new Uint8Array(),
        tree: new Uint8Array(),
      },
    });
  });
});

describe("Canister - ws_get_messages (receive)", () => {
  beforeAll(async () => {
    await assignKeyPairsToClients();

    await wsRegister({
      clientActor: client1,
      clientKey: client1KeyPair.publicKey,
    }, true);

    await wsOpen({
      clientPublicKey: client1KeyPair.publicKey,
      clientSecretKey: client1KeyPair.secretKey,
      canisterId,
      gatewayActor: gateway1,
    }, true);

    // prepare the messages
    for (let i = 0; i < SEND_MESSAGES_COUNT; i++) {
      const appMessage = { text: `test${i}` };
      await wsSend({
        clientPublicKey: client1KeyPair.publicKey,
        actor: client1,
        message: appMessage,
      }, true);
    }

    await commonAgent.fetchRootKey();
  });

  afterAll(async () => {
    await wsWipe(gateway1);
  });

  it("registered gateway can receive correct amount of messages", async () => {
    for (let i = 0; i < SEND_MESSAGES_COUNT; i++) {
      const res = await gateway1.ws_get_messages({
        nonce: BigInt(i),
      });

      expect(res).toMatchObject<CanisterWsGetMessagesResult>({
        Ok: {
          messages: expect.any(Array),
          cert: expect.any(Uint8Array),
          tree: expect.any(Uint8Array),
        },
      });

      const messagesResult = (res as { Ok: CanisterOutputCertifiedMessages }).Ok;
      expect(messagesResult.messages.length).toBe(
        SEND_MESSAGES_COUNT - i > MAX_NUMBER_OF_RETURNED_MESSAGES
          ? MAX_NUMBER_OF_RETURNED_MESSAGES
          : SEND_MESSAGES_COUNT - i
      );
    }

    // try to get more messages than available
    const res = await gateway1.ws_get_messages({
      nonce: BigInt(SEND_MESSAGES_COUNT),
    });

    expect(res).toMatchObject<CanisterWsGetMessagesResult>({
      Ok: {
        messages: [],
        cert: expect.any(Uint8Array),
        tree: expect.any(Uint8Array),
      },
    });
  });

  it("registered gateway can receive certified messages", async () => {
    // first batch of messages
    const firstBatchRes = await gateway1.ws_get_messages({
      nonce: BigInt(0),
    });

    const firstBatchMessagesResult = (firstBatchRes as { Ok: CanisterOutputCertifiedMessages }).Ok;
    for (let i = 0; i < firstBatchMessagesResult.messages.length; i++) {
      const message = firstBatchMessagesResult.messages[i];
      const decodedVal = Cbor.decode<WebsocketMessage>(new Uint8Array(message.val));
      expect(decodedVal).toMatchObject<WebsocketMessage>({
        client_key: client1KeyPair.publicKey,
        message: Cbor.encode({ text: `test${i}` }),
        sequence_num: i + 1,
        timestamp: expect.any(Object), // weird deserialization of timestamp
      });

      // check the certification
      await expect(
        isValidCertificate(
          canisterId,
          firstBatchMessagesResult.cert as Uint8Array,
          firstBatchMessagesResult.tree as Uint8Array,
          commonAgent
        )
      ).resolves.toBe(true);
      await expect(
        isMessageBodyValid(
          message.key,
          message.val as Uint8Array,
          firstBatchMessagesResult.tree as Uint8Array,
        )
      ).resolves.toBe(true);
    }

    // second batch of messages, starting from the last nonce of the first batch
    const secondBatchRes = await gateway1.ws_get_messages({
      nonce: BigInt(MAX_NUMBER_OF_RETURNED_MESSAGES),
    });

    const secondBatchMessagesResult = (secondBatchRes as { Ok: CanisterOutputCertifiedMessages }).Ok;
    for (let i = 0; i < secondBatchMessagesResult.messages.length; i++) {
      const message = secondBatchMessagesResult.messages[i];
      const decodedVal = Cbor.decode<WebsocketMessage>(new Uint8Array(message.val));
      expect(decodedVal).toMatchObject<WebsocketMessage>({
        client_key: client1KeyPair.publicKey,
        message: Cbor.encode({ text: `test${i + MAX_NUMBER_OF_RETURNED_MESSAGES}` }),
        sequence_num: i + MAX_NUMBER_OF_RETURNED_MESSAGES + 1,
        timestamp: expect.any(Object), // weird deserialization of timestamp
      });

      // check the certification
      await expect(
        isValidCertificate(
          canisterId,
          secondBatchMessagesResult.cert as Uint8Array,
          secondBatchMessagesResult.tree as Uint8Array,
          commonAgent
        )
      ).resolves.toBe(true);
      await expect(
        isMessageBodyValid(
          message.key,
          message.val as Uint8Array,
          secondBatchMessagesResult.tree as Uint8Array,
        )
      ).resolves.toBe(true);
    }
  });
});

describe("Canister - ws_close", () => {
  beforeAll(async () => {
    await assignKeyPairsToClients();

    await wsRegister({
      clientActor: client1,
      clientKey: client1KeyPair.publicKey,
    }, true);

    await wsOpen({
      clientPublicKey: client1KeyPair.publicKey,
      clientSecretKey: client1KeyPair.secretKey,
      canisterId,
      gatewayActor: gateway1,
    }, true);
  });

  afterAll(async () => {
    await wsWipe(gateway1);
  });

  it("fails if gateway is not registered", async () => {
    const res = await wsClose({
      clientPublicKey: client1KeyPair.publicKey,
      gatewayActor: gateway2,
    });

    expect(res).toMatchObject<CanisterWsCloseResult>({
      Err: "caller is not the gateway that has been registered during CDK initialization",
    });
  });

  it("fails if client is not registered", async () => {
    const res = await wsClose({
      clientPublicKey: client2KeyPair.publicKey,
      gatewayActor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsCloseResult>({
      Err: "client's public key has not been previously registered by client",
    });
  });

  it("should close the websocket for a registered client", async () => {
    const res = await wsClose({
      clientPublicKey: client1KeyPair.publicKey,
      gatewayActor: gateway1,
    });

    expect(res).toMatchObject<CanisterWsCloseResult>({
      Ok: null,
    });
  });
});

describe("Canister - ws_send", () => {
  beforeAll(async () => {
    await assignKeyPairsToClients();

    await wsRegister({
      clientActor: client1,
      clientKey: client1KeyPair.publicKey,
    }, true);

    await wsOpen({
      clientPublicKey: client1KeyPair.publicKey,
      clientSecretKey: client1KeyPair.secretKey,
      canisterId,
      gatewayActor: gateway1,
    }, true);
  });

  afterAll(async () => {
    await wsWipe(gateway1);
  });

  it("fails if sending a message to a non registered client", async () => {
    const res = await wsSend({
      clientPublicKey: client2KeyPair.publicKey,
      actor: client1,
      message: { text: "test" },
    });

    expect(res).toMatchObject<CanisterWsSendResult>({
      Err: "outgoing message to client num not initialized for client",
    });
  });

  it("should send a message to a registered client", async () => {
    const res = await wsSend({
      clientPublicKey: client1KeyPair.publicKey,
      actor: client1,
      message: { text: "test" },
    });

    expect(res).toMatchObject<CanisterWsSendResult>({
      Ok: null,
    });
  });
});

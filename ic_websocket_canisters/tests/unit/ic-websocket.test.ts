import WS from "jest-websocket-mock";
import { rest } from "msw";
import { setupServer } from "msw/node";
import { Cbor, compare, fromHex } from "@dfinity/agent";
import * as ed from '@noble/ed25519';

import IcWebSocket from "../../src/ic_websocket_frontend/src/ic-websocket/icWebsocket";
import type { IcWebSocketConfig } from "../../src/ic_websocket_frontend/src/ic-websocket/icWebsocket";
import { Principal } from "@dfinity/principal";

const wsGatewayAddress = "ws://localhost:8080";
// the canister from which the application message was sent (needed to verify the message certificate)
const canisterId = Principal.fromText("bnz7o-iuaaa-aaaaa-qaaaa-cai");
const icNetworkUrl = "http://localhost";
const mockCanisterActor = {
  ws_message: jest.fn(),
  ws_register: jest.fn(),
};

const icWebsocketConfig: IcWebSocketConfig<any> = {
  canisterActor: mockCanisterActor,
  canisterId: canisterId.toText(),
  networkUrl: icNetworkUrl,
  localTest: true,
  persistKey: true,
};

//// Mock Servers
let mockWsServer: WS;
const mockReplica = setupServer(
  rest.get(`${icNetworkUrl}/api/v2/status`, (req, res, ctx) => {
    return res(
      ctx.status(200),
      // this response was generated from a local replica
      // used for the same integration test as the application message below
      ctx.body(fromHex("d9d9f7a66e69635f6170695f76657273696f6e66302e31382e3068726f6f745f6b65795885308182301d060d2b0601040182dc7c0503010201060c2b0601040182dc7c05030201036100b12bac4b112b66bc98e54f85e3c1bd3505f9dc08c3815e71fdbfa8718a1288738fb03539eb2fc8e3eb6bbbf40abdc5490dde09090c268084a1e24c0a70d85d4f258373fc6ef5532aac2c1be153bcd319bc02f677069f6296713391a5ae3ba4346c696d706c5f76657273696f6e65302e382e3069696d706c5f68617368784030343366663064393237626337313431643761643630616235646331313934636364303164393761386431633333393632643236663730323461646463336135757265706c6963615f6865616c74685f737461747573676865616c746879706365727469666965645f686569676874181b")),
    );
  }),
);

describe("IcWebsocket class", () => {
  beforeAll(() => {
    // start the mock worker
    mockReplica.listen();
  });

  afterAll(() => {
    localStorage.clear();
    // stop the mock worker
    mockReplica.close();
  });

  beforeEach(() => {
    mockWsServer = new WS("ws://localhost:8080");
  });

  afterEach(() => {
    mockWsServer.close();
  });

  it("throws an error if the canisterActor does not implement ws_register", async () => {
    const icWsConfig: any = {
      ...icWebsocketConfig,
      canisterActor: {
        ws_message: jest.fn(),
      },
    };

    expect(() => new IcWebSocket(wsGatewayAddress, undefined, icWsConfig)).toThrowError("Canister actor does not implement the ws_register method");
  });

  it("throws an error if the canisterActor does not implement ws_message", async () => {
    const icWsConfig: any = {
      ...icWebsocketConfig,
      canisterActor: {
        ws_register: jest.fn(),
      },
    };

    expect(() => new IcWebSocket(wsGatewayAddress, undefined, icWsConfig)).toThrowError("Canister actor does not implement the ws_message method");
  });

  it("stores the private key if persistKey is true", async () => {
    expect(localStorage.getItem("ic_websocket_client_secret_key")).toBeNull();

    const icWsConfig: any = {
      ...icWebsocketConfig,
      persistKey: true,
    };

    const icWs = new IcWebSocket(wsGatewayAddress, undefined, icWsConfig);
    expect(icWs).toBeDefined();
    expect(localStorage.getItem("ic_websocket_client_secret_key")).not.toBeNull();
  });

  it("loads the private key if persistKey is true", async () => {
    const icWsConfig: any = {
      ...icWebsocketConfig,
      persistKey: true,
    };

    const icWs = new IcWebSocket(wsGatewayAddress, undefined, icWsConfig);
    expect(icWs).toBeDefined();
    expect(localStorage.getItem("ic_websocket_client_secret_key")).not.toBeNull();
  });

  it("does not store the private key if persistKey is false", async () => {
    // clear the private key from memory from the previous test
    localStorage.removeItem("ic_websocket_client_secret_key");

    const icWsConfig: any = {
      ...icWebsocketConfig,
      persistKey: false,
    };

    const icWs = new IcWebSocket(wsGatewayAddress, undefined, icWsConfig);
    expect(icWs).toBeDefined();
    expect(localStorage.getItem("ic_websocket_client_secret_key")).toBeNull();
  });

  it("create a new instance and send first message", async () => {
    const icWs = new IcWebSocket(wsGatewayAddress, undefined, icWebsocketConfig);
    expect(icWs).toBeDefined();
    await mockWsServer.connected;

    // reconstruct the message that the client should send
    const clientSecretKey = localStorage.getItem("ic_websocket_client_secret_key");
    const publicKey = await ed.getPublicKeyAsync(clientSecretKey);
    const cborContent = Cbor.encode({
      client_key: publicKey,
      canister_id: canisterId,
    });
    const toSign = new Uint8Array(cborContent);
    const sig = await ed.signAsync(toSign, clientSecretKey);
    const message = Cbor.encode({
      content: toSign,
      sig: sig,
    });

    // get the first message sent by the client from the mock websocket server
    const firstMessage = await mockWsServer.nextMessage as ArrayBuffer;

    expect(compare(firstMessage, message)).toEqual(0);
  });

  it("onopen is called when the the ws gateway sends the first message", async () => {
    const onOpen = jest.fn();
    const icWs = new IcWebSocket(wsGatewayAddress, undefined, icWebsocketConfig);
    expect(icWs).toBeDefined();
    icWs.onopen = onOpen;
    await mockWsServer.connected;

    expect(onOpen).not.toHaveBeenCalled();

    // wait for the first message from the client
    await mockWsServer.nextMessage;
    // send the first message from the server
    mockWsServer.send("1");

    expect(onOpen).toHaveBeenCalled();
  });

  it("onmessage is called when the the ws gateway sends a message", async () => {
    const onMessage = jest.fn();
    const onError = jest.fn();
    const icWs = new IcWebSocket(wsGatewayAddress, undefined, icWebsocketConfig);
    expect(icWs).toBeDefined();
    icWs.onmessage = onMessage;
    icWs.onerror = onError;
    await mockWsServer.connected;

    // wait for the first message from the client
    await mockWsServer.nextMessage;
    // send the first message from the server
    mockWsServer.send("1");

    expect(onMessage).not.toHaveBeenCalled();

    // send an application message from the server
    // this was generated by an integration test in a local replica
    const applicationMessage = {
      key: "sqdfl-mr4km-2hfjy-gajqo-xqvh7-hf4mf-nra4i-3it6l-neaw4-soolw-tae_00000000000000000000",
      val: fromHex("d9d9f7a46a636c69656e745f6b657958205bf358639ab48f2d6fb6a0a48854405278bddeaaed6f58302ebad66e2d4a7de36c73657175656e63655f6e756d016974696d657374616d701b17748882ece5ad12676d6573736167654ed9d9f7a164746578746470696e67"),
      cert: fromHex("d9d9f7a2647472656583018301830183024863616e6973746572830183024a8000000000100000010183018301830183024e6365727469666965645f646174618203582087be17d7ba688bc4fb13d61f3d8d5642df92536e40da98facba7ba0450ea59e082045820f8d20e36feb79f8495eb4c632b7a04171599957c8c267f55b2156d89b5c1e424820458200d3dc76c69e69f12678a2e9a5a6859579ceb28350608e69f1902055650edaf7f820458209e5663705fa61a6ca53ee4a61daa4621e8ece0febd99b334d0ae625aad9f3f6e8204582077d28a3053cd3845a065a879ce36849add41cae9e6a56d452e328194b38e15ec82045820932e7cd3d24b95ff6c0fea6086fc624ee43b0875101777e089ba915ef7ded93d82045820fbb733f900879885afed4ace8eb90b19245f8386e7537769c072e9fded13c6ad830182045820ae29f371a7ae2a4af8bb125ef23486745500f8cb31a02c35cc7155053b67cce683024474696d6582034992da96e7ae90a2ba17697369676e61747572655830932828f169fdce98969c417666f38002e2826081deb756e94c73091a1b7c0455f6c2f3ccc509ab576c8e6f8753b7d85b"),
      tree: fromHex("d9d9f7830249776562736f636b657483025854737164666c2d6d72346b6d2d3268666a792d67616a716f2d78717668372d6866346d662d6e726134692d336974366c2d6e656177342d736f6f6c772d7461655f303030303030303030303030303030303030303082035820215b2aae42ccf90c6fd928fec029d0b9308e97d0c40f8772100a30097b004bb5"),
    };
    mockWsServer.send(Cbor.encode(applicationMessage));

    await expect(new Promise<void>((resolve) => {
      icWs.onmessage = (ev) => {
        console.log("onmessage", ev.data);
        resolve();
      };
    })).resolves.not.toThrow();

    expect(onError).not.toHaveBeenCalled();
  });

  it("onerror is called when the application message is invalid", async () => {
    const onMessage = jest.fn();
    const onError = jest.fn();
    const icWs = new IcWebSocket(wsGatewayAddress, undefined, icWebsocketConfig);
    expect(icWs).toBeDefined();
    icWs.onmessage = onMessage;
    icWs.onerror = onError;
    await mockWsServer.connected;

    expect(onError).not.toHaveBeenCalled();

    // wait for the first message from the client
    await mockWsServer.nextMessage;
    // send the first message from the server
    mockWsServer.send("1");

    expect(onError).not.toHaveBeenCalled();

    // send an invalid application message from the server
    const invalidApplicationMessage = {
      // the key has been modified to be invalid
      key: "wrong-key",
      val: fromHex("d9d9f7a46a636c69656e745f6b657958205bf358639ab48f2d6fb6a0a48854405278bddeaaed6f58302ebad66e2d4a7de36c73657175656e63655f6e756d016974696d657374616d701b17748882ece5ad12676d6573736167654ed9d9f7a164746578746470696e67"),
      cert: fromHex("d9d9f7a2647472656583018301830183024863616e6973746572830183024a8000000000100000010183018301830183024e6365727469666965645f646174618203582087be17d7ba688bc4fb13d61f3d8d5642df92536e40da98facba7ba0450ea59e082045820f8d20e36feb79f8495eb4c632b7a04171599957c8c267f55b2156d89b5c1e424820458200d3dc76c69e69f12678a2e9a5a6859579ceb28350608e69f1902055650edaf7f820458209e5663705fa61a6ca53ee4a61daa4621e8ece0febd99b334d0ae625aad9f3f6e8204582077d28a3053cd3845a065a879ce36849add41cae9e6a56d452e328194b38e15ec82045820932e7cd3d24b95ff6c0fea6086fc624ee43b0875101777e089ba915ef7ded93d82045820fbb733f900879885afed4ace8eb90b19245f8386e7537769c072e9fded13c6ad830182045820ae29f371a7ae2a4af8bb125ef23486745500f8cb31a02c35cc7155053b67cce683024474696d6582034992da96e7ae90a2ba17697369676e61747572655830932828f169fdce98969c417666f38002e2826081deb756e94c73091a1b7c0455f6c2f3ccc509ab576c8e6f8753b7d85b"),
      tree: fromHex("d9d9f7830249776562736f636b657483025854737164666c2d6d72346b6d2d3268666a792d67616a716f2d78717668372d6866346d662d6e726134692d336974366c2d6e656177342d736f6f6c772d7461655f303030303030303030303030303030303030303082035820215b2aae42ccf90c6fd928fec029d0b9308e97d0c40f8772100a30097b004bb5"),
    };
    mockWsServer.send(Cbor.encode(invalidApplicationMessage));

    await expect(new Promise<void>((resolve) => {
      icWs.onerror = (ev) => {
        console.log("onerror", ev);
        resolve();
      };
    })).resolves.not.toThrow();

    expect(onMessage).not.toHaveBeenCalled();
  });
});
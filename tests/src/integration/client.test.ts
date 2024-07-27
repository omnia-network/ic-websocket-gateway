import IcWebSocket, { createWsConfig, generateRandomIdentity } from "ic-websocket-js";
import { test_canister_rs } from "../declarations/test_canister_rs";
// import { test_canister_mo } from "../declarations/test_canister_mo";
import type { AppMessage, _SERVICE } from "../declarations/test_canister_rs/test_canister_rs.did";
// import type { AppMessage, _SERVICE } from "../declarations/test_canister_mo/test_canister_mo.did";

/// IcWebsocket parameters
const gatewayAddress = process.env.WS_GATEWAY_URL;
const icUrl = process.env.IC_URL;
const canisterId = process.env.TEST_CANISTER_ID
  || process.env.CANISTER_ID_TEST_CANISTER_RS
  || process.env.CANISTER_ID_TEST_CANISTER_MO;

console.log("gatewayAddress", gatewayAddress);
console.log("icUrl", icUrl);
console.log("canisterId", canisterId);

/// test constants & variables
const pingPongCount = 20;
let ws: IcWebSocket<_SERVICE, AppMessage>;

/// jest configuration
jest.setTimeout(180_000);

/// helpers
// get the indices of a substring in a string
const getIndicesOf = (searchStr: string, str: string) => {
  var searchStrLen = searchStr.length;
  if (searchStrLen == 0) {
    return [];
  }

  let startIndex = 0;
  let index = 0;
  const indices: number[] = [];

  while ((index = str.indexOf(searchStr, startIndex)) > -1) {
    indices.push(index);
    startIndex = index + searchStrLen;
  }

  return indices;
};

// reconstruct the ws message from the index
const reconstructWsMessage = (index: number) => {
  let message = "ping";

  for (let i = 0; i < index; i++) {
    message += "-pong ping";
  }

  return message;
}

/// tests
describe("WS client", () => {
  it("should open a connection", async () => {
    const wsConfig = createWsConfig({
      canisterId,
      canisterActor: test_canister_rs,
      // canisterActor: test_canister_mo,
      networkUrl: icUrl,
      identity: generateRandomIdentity(),
    });

    ws = new IcWebSocket(gatewayAddress, undefined, wsConfig);

    ws.onopen = () => {
      // No assertion needed, the test will pass as long as the onopen event fires
    };

    ws.onerror = (event) => {
      console.log("IcWebSocket error", event);
      expect(event).toBeNull();
    };
  });

  it("should send and receive messages", async () => {
    let messageCounter = 0;
    // wrap the test in a promise so we can await it before ending the test

    const promisifiedHandlers = new Promise<void>((resolve, reject) => {
      ws.onerror = (event) => {
        console.error("IcWebSocket error:", event.error);
        reject(event.error);
      };

      ws.onmessage = async (event) => {
        const message = event.data;
        if (!(message.text === reconstructWsMessage(messageCounter))) {
          return reject("Received message does not match expected message");
        }

        messageCounter++;
        const indices = getIndicesOf("ping", message.text);
        if (messageCounter === pingPongCount) {
          // close the test after the last message
          return resolve();
        }
        expect(indices.length).toBe(messageCounter);

        const pongMessage: AppMessage = {
          text: message.text + "-pong",
          timestamp: BigInt(Date.now()),
        };

        ws.send(pongMessage);
      };
    });

    await expect(promisifiedHandlers).resolves.not.toThrow();
  });

  it("should close the connection", async () => {
    // wrap the test in a promise so we can await it before ending the test
    const promisifiedHandlers = new Promise<void>((resolve, reject) => {
      ws.onerror = (event) => {
        console.error("IcWebSocket error:", event.error);
        reject(event.error);
      };

      ws.onclose = () => {
        // close the test after closing the connection
        resolve();
      };

      ws.close();
    });

    await expect(promisifiedHandlers).resolves.not.toThrow();
  });
});

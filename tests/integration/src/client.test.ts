import IcWebSocket, { generateRandomIdentity } from "ic-websocket-js";
import environment from "./utils/environment";
import { deserializeAppMessage, serializeAppMessage } from "./utils/idl";
import type { _SERVICE } from "../../test_canister_rs/src/declarations/test_canister_rs/test_canister_rs.did";

/// IcWebsocket parameters
const gatewayAddress = environment.WS_GATEWAY_URL;
const icUrl = environment.IC_URL;
const canisterId = environment.TEST_CANISTER_ID;

/// test constants & variables
const pingPongCount = 20;
let ws: IcWebSocket;

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
    ws = new IcWebSocket(gatewayAddress, undefined, {
      canisterId,
      networkUrl: icUrl,
      identity: generateRandomIdentity(),
      ackMessageTimeout: 19_000, // the ack interval is set to 10 seconds on the CDK and the keep alive timeout is set to 9 second
    });

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
        const message = deserializeAppMessage(event.data);
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

        const pongMessage = serializeAppMessage({
          text: message.text + "-pong",
          timestamp: BigInt(Date.now()),
        });

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

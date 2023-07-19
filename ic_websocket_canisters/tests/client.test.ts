import { ic_websocket_backend } from "../src/declarations/ic_websocket_backend";
import IcWebSocket from "../src/ic_websocket_frontend/src/icWebsocket";

/// IcWebsocket parameters
const backendCanisterId = process.env.CANISTER_ID_IC_WEBSOCKET_BACKEND || "";
const gatewayAddress = "ws://127.0.0.1:8080";
const url = "http://127.0.0.1:4943";
const localTest = true;
const persistKey = false;

/// test constants & variables
const pingPongCount = 5;
let ws: IcWebSocket;

/// jest configuration
jest.setTimeout(30_000);

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

// TODO: make ws gateway and local replica start automatically before running tests

/// tests
describe("WS client", () => {
  it("should open a connection", async () => {
    ws = new IcWebSocket(gatewayAddress, undefined, {
      canisterActor: ic_websocket_backend,
      canisterId: backendCanisterId,
      networkUrl: url,
      localTest,
      persistKey,
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
    await new Promise<void>((resolve) => {
      ws.onerror = (event) => {
        console.log("IcWebSocket error", event);
        expect(event).toBeNull();
      };

      ws.onmessage = (event) => {
        expect(event.data).toEqual(reconstructWsMessage(messageCounter));

        messageCounter++;
        const indices = getIndicesOf("ping", event.data);
        if (messageCounter === pingPongCount) {
          // close the test after the last message
          return resolve();
        }
        expect(indices.length).toBe(messageCounter);

        ws.send({
          text: event.data + "-pong",
        });
      };
    });
  });

  it("should close the connection", async () => {
    // wrap the test in a promise so we can await it before ending the test
    await new Promise<void>((resolve) => {
      ws.onerror = (event) => {
        console.log("IcWebSocket error", event);
        expect(event).toBeNull();
      };

      ws.onclose = () => {
        // close the test after closing the connection
        return resolve();
      };

      ws.close();
    });
  });
});

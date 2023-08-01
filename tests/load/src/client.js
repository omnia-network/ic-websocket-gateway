require('./prepare-environment.js');

const IcWebsocket = require('ic-websocket-js/lib/cjs/index.js').default;
const { createActor } = require('./helpers.js');

const {
  WS_GATEWAY_URL,
  IC_URL,
  TEST_CANISTER_ID,
  FETCH_IC_ROOT_KEY,
} = process.env;

const test_canister = createActor(TEST_CANISTER_ID, {
  agentOptions: {
    host: IC_URL,
  }
});

async function connectClient(userContext, events, next) {
  try {
    const startTimestamp = Date.now();
    const customWs = new IcWebsocket(WS_GATEWAY_URL, undefined, {
      canisterActor: test_canister,
      canisterId: TEST_CANISTER_ID,
      networkUrl: IC_URL,
      persistKey: false,
      localTest: FETCH_IC_ROOT_KEY === 'true',
    });

    await new Promise((resolve, reject) => {
      customWs.onopen = () => {
        events.emit('histogram', 'open_connection_latency_s', (Date.now() - startTimestamp) / (10 ** 3));
        events.emit('counter', 'connect_client_success', 1);
        resolve();
      };

      customWs.onerror = (event) => {
        reject(event.error);
      }

      userContext.vars.ws = customWs;
    });

    next();
  } catch (err) {
    console.error(err);
    events.emit('counter', 'connect_client_error', 1);

    next(new Error(err));
  }
}

async function sendMessages(userContext, events, next) {
  const ws = userContext.vars.ws;

  try {
    await new Promise((resolve, reject) => {
      ws.onerror = (event) => {
        reject(event.error);
      };

      let messageCounter = 0;

      ws.onmessage = async (event) => {
        messageCounter++;
        if (messageCounter === 5) {
          // close the test after the last message
          return resolve();
        }

        events.emit('histogram', 'receive_message_latency_s', (Date.now() * (10 ** 6) - event.data.timestamp) / (10 ** 9));

        try {
          await ws.send({
            text: event.data.text + "-pong",
            timestamp: Date.now(),
          });

          events.emit('counter', 'send_message_success', 1);
        } catch (err) {
          reject(err);
        }
      };
    });

    next();
  } catch (err) {
    console.error(err);

    events.emit('counter', 'send_message_error', 1);

    next(new Error(err));
  }
}

function disconnectClient(userContext, events, done) {
  userContext.vars.ws.close();

  events.emit('counter', 'disconnect_client', 1);

  return done();
}

module.exports = { connectClient, sendMessages, disconnectClient };

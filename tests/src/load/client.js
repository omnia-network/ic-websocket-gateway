require('./prepare-environment.js');

const { default: IcWebsocket, generateRandomIdentity, createWsConfig } = require('ic-websocket-js/lib/cjs/index.js');
const { test_canister_rs } = require('../declarations/test_canister_rs/index.js');

const {
  WS_GATEWAY_URL,
  IC_URL,
  TEST_CANISTER_ID,
} = process.env;

const messagesSentPerClient = 20;

async function connectClient(userContext, events, next) {
  try {
    const startTimestamp = Date.now();

    const wsConfig = createWsConfig({
      canisterId: TEST_CANISTER_ID,
      canisterActor: test_canister_rs,
      networkUrl: IC_URL,
      identity: generateRandomIdentity(),
      ackMessageTimeout: 450_000, // the ack interval is set to 300 seconds on the CDK and the keep alive timeout is set to 60 second
    });
    const customWs = new IcWebsocket(WS_GATEWAY_URL, undefined, wsConfig);

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

      ws.onmessage = (event) => {
        messageCounter++;
        if (messageCounter === messagesSentPerClient) {
          // close the test after the last message
          return resolve();
        }

        const message = event.data;

        events.emit(
          'histogram',
          'receive_message_latency_ms',
          Number(BigInt(Date.now()) - (BigInt(message.timestamp) / BigInt(10 ** 6)))
        );

        try {
          const messageToSend = {
            text: message.text + "-pong",
            timestamp: Date.now(),
          };
          ws.send(messageToSend);

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

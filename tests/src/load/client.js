require('./prepare-environment.js');

const { default: IcWebsocket, generateRandomIdentity, createWsConfig } = require('ic-websocket-js/lib/cjs/index.js');
const { test_canister_rs } = require('../declarations/test_canister_rs/index.js');

/// IcWebsocket parameters
const gatewayAddress = process.env.WS_GATEWAY_URL;
const icUrl = process.env.IC_URL;
const canisterId = process.env.TEST_CANISTER_ID
  || process.env.CANISTER_ID_TEST_CANISTER_RS
  || process.env.CANISTER_ID_TEST_CANISTER_MO;

const messagesSentPerClient = 20;

async function connectClient(userContext, events) {
  try {
    const startTimestamp = Date.now();

    const wsConfig = createWsConfig({
      canisterId: canisterId,
      canisterActor: test_canister_rs,
      networkUrl: icUrl,
      identity: generateRandomIdentity(),
    });
    const customWs = new IcWebsocket(gatewayAddress, undefined, wsConfig);

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
  } catch (err) {
    console.error(err);
    events.emit('counter', 'connect_client_error', 1);

    throw new Error(err);
  }
}

async function sendMessages(userContext, events) {
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
  } catch (err) {
    console.error(err);
    events.emit('counter', 'send_message_error', 1);

    throw new Error(err);
  }
}

function disconnectClient(userContext, events, done) {
  userContext.vars.ws.close();

  events.emit('counter', 'disconnect_client', 1);

  done();
}

module.exports = { connectClient, sendMessages, disconnectClient };

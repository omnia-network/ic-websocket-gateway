const crypto = require('isomorphic-webcrypto').default;
const WebSocket = require('ws');
const dotenv = require('dotenv');

// load .env file
dotenv.config();

// polyfill crypto
Object.defineProperty(globalThis, 'crypto', {
  value: {
    getRandomValues: crypto.getRandomValues,
    subtle: crypto.subtle,
  },
});

// polyfill WebSocket
globalThis.WebSocket = WebSocket;

// these WebSocket-related events are web-only, so we need to polyfill them
class ErrorEvent extends Event {
  error;
  message;

  constructor(type, init = {}) {
    super(type, init);
  }
}
globalThis.ErrorEvent = ErrorEvent;

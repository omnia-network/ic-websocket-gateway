const IDL = require("@dfinity/candid/lib/cjs/idl.js");

const AppMessageIdl = IDL.Record({
  'text': IDL.Text,
  'timestamp': IDL.Nat64,
});

const serializeAppMessage = (message) => {
  return new Uint8Array(IDL.encode([AppMessageIdl], [message]));
};

const deserializeAppMessage = (bytes) => {
  return IDL.decode([AppMessageIdl], bytes)[0];
};

module.exports = {
  serializeAppMessage,
  deserializeAppMessage,
};

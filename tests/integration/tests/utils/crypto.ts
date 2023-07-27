import * as ed from "@noble/ed25519";

export const getKeyPair = async (secretKey?: string | Uint8Array): Promise<{ publicKey: Uint8Array, secretKey: Uint8Array | string }> => {
  if (!secretKey) {
    secretKey = ed.utils.randomPrivateKey();
  }

  const publicKey = await ed.getPublicKeyAsync(secretKey);

  return {
    publicKey,
    secretKey,
  };
};

export const getSignedMessage = async (buf: ArrayBuffer | Uint8Array, secretKey: Uint8Array | string): Promise<{ content: Uint8Array; sig: Uint8Array; }> => {
  // Sign the message so that the gateway can verify canister and client ids match
  const toSign = new Uint8Array(buf);
  const sig = await ed.signAsync(toSign, secretKey);

  // Final signed websocket message
  const message = {
    content: toSign,
    sig: sig,
  };

  return message;
}

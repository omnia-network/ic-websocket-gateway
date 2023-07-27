// helpers for functions that are called frequently in tests

import { ActorSubclass, Cbor, Certificate, HashTree, HttpAgent, compare, lookup_path, reconstruct } from "@dfinity/agent";
import { CanisterIncomingMessage, ClientPublicKey, _SERVICE } from "../../src/declarations/test_canister/test_canister.did";
import { getSignedMessage } from "./crypto";
import { Secp256k1KeyIdentity } from "@dfinity/identity-secp256k1";
import { Principal } from "@dfinity/principal";

type WsRegisterArgs = {
  clientActor: ActorSubclass<_SERVICE>,
  clientKey: Uint8Array,
};

export const wsRegister = async (args: WsRegisterArgs, throwIfError = false) => {
  const res = await args.clientActor.ws_register({
    client_key: args.clientKey,
  });

  if (throwIfError) {
    if ('Err' in res) {
      throw new Error(res.Err);
    }
  }

  return res;
};

type WsOpenArgs = {
  clientPublicKey: Uint8Array,
  clientSecretKey: Uint8Array | string,
  canisterId: string,
  gatewayActor: ActorSubclass<_SERVICE>,
};

export const wsOpen = async (args: WsOpenArgs, throwIfError = false) => {
  const content = Cbor.encode({
    client_key: args.clientPublicKey,
    canister_id: args.canisterId,
  });
  const signedMessage = await getSignedMessage(content, args.clientSecretKey);

  const res = await args.gatewayActor.ws_open({
    msg: signedMessage.content,
    sig: signedMessage.sig,
  });

  if (throwIfError) {
    if ('Err' in res) {
      throw new Error(res.Err);
    }
  }

  return res;
};

type WsMessageArgs = {
  message: CanisterIncomingMessage,
  actor: ActorSubclass<_SERVICE>,
};

export const wsMessage = async (args: WsMessageArgs, throwIfError = false) => {
  const res = await args.actor.ws_message({
    msg: args.message,
  });

  if (throwIfError) {
    if ('Err' in res) {
      throw new Error(res.Err);
    }
  }

  return res;
};

export type WebsocketMessage = {
  client_key: ClientPublicKey,
  sequence_num: number,
  timestamp: number,
  message: ArrayBuffer | Uint8Array,
};

export const getWebsocketMessage = (clientPublicKey: ClientPublicKey, sequenceNumber: number, content?: ArrayBuffer | Uint8Array): Uint8Array => {
  const websocketMessage: WebsocketMessage = {
    client_key: clientPublicKey,
    sequence_num: sequenceNumber,
    timestamp: Date.now(),
    message: new Uint8Array(content || [1, 2, 3, 4]),
  };

  return new Uint8Array(Cbor.encode(websocketMessage));
};

type WsCloseArgs = {
  clientPublicKey: Uint8Array,
  gatewayActor: ActorSubclass<_SERVICE>,
};

export const wsClose = async (args: WsCloseArgs, throwIfError = false) => {
  const res = await args.gatewayActor.ws_close({
    client_key: args.clientPublicKey,
  });

  if (throwIfError) {
    if ('Err' in res) {
      throw new Error(res.Err);
    }
  }

  return res;
};

export const wsWipe = async (gatewayActor: ActorSubclass<_SERVICE>) => {
  await gatewayActor.ws_wipe();
};

type WsSendArgs = {
  clientPublicKey: Uint8Array,
  actor: ActorSubclass<_SERVICE>,
  message: {
    text: string,
  },
};

export const wsSend = async (args: WsSendArgs, throwIfError = false) => {
  const res = await args.actor.ws_send(args.clientPublicKey, args.message);

  if (throwIfError) {
    if ('Err' in res) {
      throw new Error(res.Err);
    }
  }

  return res;
};

export const getCertifiedMessageKey = async (gatewayIdentity: Promise<Secp256k1KeyIdentity>, nonce: number) => {
  const gatewayPrincipal = (await gatewayIdentity).getPrincipal().toText();
  return `${gatewayPrincipal}_${String(nonce).padStart(20, '0')}`;
};


export const isValidCertificate = async (canisterId: string, certificate: Uint8Array, tree: Uint8Array, agent: HttpAgent) => {
  const canisterPrincipal = Principal.fromText(canisterId);
  let cert: Certificate;

  try {
    cert = await Certificate.create({
      certificate,
      canisterId: canisterPrincipal,
      rootKey: agent.rootKey!
    });
  } catch (error) {
    console.error("Error creating certificate:", error);
    return false;
  }

  const hashTree = Cbor.decode<HashTree>(tree);
  const reconstructed = await reconstruct(hashTree);
  const witness = cert.lookup([
    "canister",
    canisterPrincipal.toUint8Array(),
    "certified_data"
  ]);

  if (!witness) {
    throw new Error(
      "Could not find certified data for this canister in the certificate."
    );
  }

  // First validate that the Tree is as good as the certification.
  return compare(witness, reconstructed) === 0;
};

export const isMessageBodyValid = async (path: string, body: Uint8Array, tree: Uint8Array) => {
  const hashTree = Cbor.decode<HashTree>(tree);
  const sha = await crypto.subtle.digest("SHA-256", body);
  let treeSha = lookup_path(["websocket", path], hashTree);

  if (!treeSha) {
    // Allow fallback to index path.
    treeSha = lookup_path(["websocket"], hashTree);
  }

  return !!treeSha && (compare(sha, treeSha) === 0);
};
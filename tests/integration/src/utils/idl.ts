import { IDL } from "@dfinity/candid";
import type { AppMessage } from "../../../test_canister_rs/src/declarations/test_canister_rs/test_canister_rs.did";

export const AppMessageIdl = IDL.Record({
  'text': IDL.Text,
  'timestamp': IDL.Nat64,
});

export const serializeAppMessage = (message: AppMessage): Uint8Array => {
  return new Uint8Array(IDL.encode([AppMessageIdl], [message]));
};

export const deserializeAppMessage = (bytes: Buffer | ArrayBuffer | Uint8Array): AppMessage => {
  return IDL.decode([AppMessageIdl], bytes)[0] as unknown as AppMessage;
};

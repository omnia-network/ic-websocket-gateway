import { Actor, ActorConfig, HttpAgent, HttpAgentOptions } from "@dfinity/agent";
import environment from "./environment";
// @ts-ignore
import { idlFactory } from "../../../test_canister/src/declarations/test_canister/test_canister.did";
import type { _SERVICE } from "../../../test_canister/src/declarations/test_canister/test_canister.did";

const commonAgentOptions: HttpAgentOptions = {
  host: environment.IC_URL,
};

// needed in the certificates validation
export const commonAgent = new HttpAgent(commonAgentOptions);

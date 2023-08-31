import { HttpAgent, HttpAgentOptions } from "@dfinity/agent";
import environment from "./environment";
import type { _SERVICE } from "../../../test_canister/src/declarations/test_canister/test_canister.did";

const commonAgentOptions: HttpAgentOptions = {
  host: environment.IC_URL,
};

// needed in the certificates validation
export const commonAgent = new HttpAgent(commonAgentOptions);

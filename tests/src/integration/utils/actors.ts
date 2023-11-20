import { HttpAgent, HttpAgentOptions } from "@dfinity/agent";
import environment from "./environment";
import type { _SERVICE } from "../../../test_canister_rs/src/declarations/test_canister_rs/test_canister_rs.did";

const commonAgentOptions: HttpAgentOptions = {
  host: environment.IC_URL,
};

// needed in the certificates validation
export const commonAgent = new HttpAgent(commonAgentOptions);

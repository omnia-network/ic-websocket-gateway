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

export const createActor = (canisterId: string, options: { agentOptions?: HttpAgentOptions; actorOptions?: ActorConfig } = {}) => {
  const agent = new HttpAgent({
    ...commonAgentOptions,
    ...options.agentOptions,
  });

  // Fetch root key for certificate validation during development
  if (process.env.DFX_NETWORK !== "ic") {
    agent.fetchRootKey().catch((err) => {
      console.warn(
        "Unable to fetch root key. Check to ensure that your local replica is running"
      );
      console.error(err);
    });
  }

  // Creates an actor with using the candid interface and the HttpAgent
  return Actor.createActor<_SERVICE>(idlFactory, {
    agent,
    canisterId,
    ...options.actorOptions,
  });
};

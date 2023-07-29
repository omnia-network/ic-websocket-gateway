import { Actor, ActorConfig, HttpAgent, HttpAgentOptions } from "@dfinity/agent";
import { identityFromSeed } from "./identity";
import environment from "./environment";
// @ts-ignore
import { idlFactory } from "../../../test_canister/src/declarations/test_canister/test_canister.did";
import type { _SERVICE } from "../../../test_canister/src/declarations/test_canister/test_canister.did";

const canisterId = environment.TEST_CANISTER_ID;

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

// Principal: "i3gux-m3hwt-5mh2w-t7wwm-fwx5j-6z6ht-hxguo-t4rfw-qp24z-g5ivt-2qe"
// this is the gateway registered in the local replica when deploying the canister with `npm run deploy:tests`
const gateway1Seed = "harsh shy amazing enroll reject shy smooth abandon fat start mixture capable";
export const gateway1Data = {
  identity: identityFromSeed(gateway1Seed),
};
export const gateway1 = createActor(canisterId, {
  agentOptions: {
    identity: gateway1Data.identity,
  },
});

// Principal: "trj6m-u7l6v-zilnb-2hl6a-3jfz3-asri5-mkw3k-e2tpo-5emmk-6hqxb-uae"
const gateway2Seed = "excess face evidence ecology gas van still noble grit night rubber boring";
export const gateway2Data = {
  identity: identityFromSeed(gateway2Seed),
};
export const gateway2 = createActor(canisterId, {
  agentOptions: {
    identity: gateway2Data.identity,
  },
});

// Principal: "pmisz-prtlk-b6oe6-bj4fl-6l5fy-h7c2h-so6i7-jiz2h-bgto7-piqfr-7ae"
const client1Seed = "rabbit fun moral twin food kangaroo egg among adjust pottery measure seek";
export const client1Data = {
  identity: identityFromSeed(client1Seed),
};
export const client1 = createActor(canisterId, {
  agentOptions: {
    identity: client1Data.identity,
  },
});

// Principal: "zuh6g-qnmvg-vky2t-tnob7-h4xoj-ykrcx-jqjpi-cdf3k-23i3i-ykozs-fae"
const client2Seed = "lamp myself priority trip trick drip process prize outside trophy lend common";
export const client2Data = {
  identity: identityFromSeed(client2Seed),
};
export const client2 = createActor(canisterId, {
  agentOptions: {
    identity: client2Data.identity,
  },
});

import { HttpAgent, fromHex } from "@dfinity/agent";
import { Principal } from "@dfinity/principal";
import { isMessageBodyValid } from "../../src/ic_websocket_frontend/src/ic-websocket/utils";

// the canister from which the correct data were generated
const canisterId = Principal.fromText("bnz7o-iuaaa-aaaaa-qaaaa-cai");
// the root key of the local replica on which the canister was installed
const localReplicaRootKey = fromHex("308182301d060d2b0601040182dc7c0503010201060c2b0601040182dc7c05030201036100b12bac4b112b66bc98e54f85e3c1bd3505f9dc08c3815e71fdbfa8718a1288738fb03539eb2fc8e3eb6bbbf40abdc5490dde09090c268084a1e24c0a70d85d4f258373fc6ef5532aac2c1be153bcd319bc02f677069f6296713391a5ae3ba434");
// the correct data
const messageToVerify = {
  key: "sqdfl-mr4km-2hfjy-gajqo-xqvh7-hf4mf-nra4i-3it6l-neaw4-soolw-tae_00000000000000000000",
  val: new Uint8Array(fromHex("d9d9f7a46a636c69656e745f6b657958205bf358639ab48f2d6fb6a0a48854405278bddeaaed6f58302ebad66e2d4a7de36c73657175656e63655f6e756d016974696d657374616d701b17748882ece5ad12676d6573736167654ed9d9f7a164746578746470696e67")),
  cert: fromHex("d9d9f7a2647472656583018301830183024863616e6973746572830183024a8000000000100000010183018301830183024e6365727469666965645f646174618203582087be17d7ba688bc4fb13d61f3d8d5642df92536e40da98facba7ba0450ea59e082045820f8d20e36feb79f8495eb4c632b7a04171599957c8c267f55b2156d89b5c1e424820458200d3dc76c69e69f12678a2e9a5a6859579ceb28350608e69f1902055650edaf7f820458209e5663705fa61a6ca53ee4a61daa4621e8ece0febd99b334d0ae625aad9f3f6e8204582077d28a3053cd3845a065a879ce36849add41cae9e6a56d452e328194b38e15ec82045820932e7cd3d24b95ff6c0fea6086fc624ee43b0875101777e089ba915ef7ded93d82045820fbb733f900879885afed4ace8eb90b19245f8386e7537769c072e9fded13c6ad830182045820ae29f371a7ae2a4af8bb125ef23486745500f8cb31a02c35cc7155053b67cce683024474696d6582034992da96e7ae90a2ba17697369676e61747572655830932828f169fdce98969c417666f38002e2826081deb756e94c73091a1b7c0455f6c2f3ccc509ab576c8e6f8753b7d85b"),
  tree: fromHex("d9d9f7830249776562736f636b657483025854737164666c2d6d72346b6d2d3268666a792d67616a716f2d78717668372d6866346d662d6e726134692d336974366c2d6e656177342d736f6f6c772d7461655f303030303030303030303030303030303030303082035820215b2aae42ccf90c6fd928fec029d0b9308e97d0c40f8772100a30097b004bb5"),
};

const agent = new HttpAgent();
agent.rootKey = localReplicaRootKey;

describe("Utils of the IcWebSocket", () => {
  it("should verify a message", async () => {
    const isValid = await isMessageBodyValid(
      canisterId,
      messageToVerify.key,
      messageToVerify.val,
      messageToVerify.cert,
      messageToVerify.tree,
      agent,
    );

    expect(isValid).toBe(true);
  });

  it("should throw if the canister is not valid", async () => {
    await expect(isMessageBodyValid(
      Principal.fromText("bkyz2-fmaaa-aaaaa-qaaaq-cai"), // another canister
      messageToVerify.key,
      messageToVerify.val,
      messageToVerify.cert,
      messageToVerify.tree,
      agent,
    )).rejects.toThrow("Could not find certified data for this canister in the certificate.");
  });

  it("should not verify a message if the path is not valid", async () => {
    const isValid = await isMessageBodyValid(
      canisterId,
      "not-valid-path",
      messageToVerify.val,
      messageToVerify.cert,
      messageToVerify.tree,
      agent,
    );

    expect(isValid).toBe(false);
  });

  it("should not verify a message if the body is not valid", async () => {
    const isValid = await isMessageBodyValid(
      canisterId,
      messageToVerify.key,
      new Uint8Array([0, 1, 2, 3]),
      messageToVerify.cert,
      messageToVerify.tree,
      agent,
    );

    expect(isValid).toBe(false);
  });

  it("should not verify a message if the certificate is not valid", async () => {
    const isValid = await isMessageBodyValid(
      canisterId,
      messageToVerify.key,
      messageToVerify.val,
      new Uint8Array([0, 1, 2, 3]),
      messageToVerify.tree,
      agent,
    );

    expect(isValid).toBe(false);
  });

  it("should not verify a message if the tree is not valid", async () => {
    const isValid = await isMessageBodyValid(
      canisterId,
      messageToVerify.key,
      messageToVerify.val,
      messageToVerify.cert,
      // another tree, valid but not the same as the one used to generate the data
      fromHex("d9d9f7830249776562736f636b657483025854737164666c2d6d72346b6d2d3268666a792d67616a716f2d78717668372d6866346d662d6e726134692d336974366c2d6e656177342d736f6f6c772d7461655f3030303030303030303030303030303030303030820358200a621494d244cd4426e1f3c2ac6942bb21a5312f613f534e6da5182571757403"),
      agent,
    );

    expect(isValid).toBe(false);
  });

  it("should not verify a message if the agent has another root key", async () => {
    const isValid = await isMessageBodyValid(
      canisterId,
      messageToVerify.key,
      messageToVerify.val,
      messageToVerify.cert,
      messageToVerify.tree,
      // the default agent uses the mainnet replica root key
      new HttpAgent(),
    );

    expect(isValid).toBe(false);
  });
});
const {
  WS_GATEWAY_URL,
  IC_URL,
  TEST_CANISTER_ID,
  FETCH_IC_ROOT_KEY,
} = process.env;

export default {
  WS_GATEWAY_URL,
  IC_URL,
  TEST_CANISTER_ID,
  FETCH_IC_ROOT_KEY: FETCH_IC_ROOT_KEY === "true",
};
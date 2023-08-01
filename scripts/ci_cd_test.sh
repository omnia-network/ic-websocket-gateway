#!/bin/bash

set -e

echo "Starting local replica"
dfx start --clean --background

echo "Running unit tests for gateway"
cargo test --workspace -- --test-threads=1

echo "Starting gateway in the background"
ls -al
cargo run > /dev/null 2>&1 &
gateway_pid=$!

ls -al

GATEWAY_PRINCIPAL=$(cargo run -q -p scripts --bin principal_from_key_pair "./data/key_pair")

echo "Gateway principal: $GATEWAY_PRINCIPAL"

echo "Deploying test canister"
cd tests/test_canister
dfx deploy test_canister --no-wallet --argument "(opt \"$GATEWAY_PRINCIPAL\")"

# source the canister id from the generated .env file
source .env

cd ../integration

echo "Running integration test"
WS_GATEWAY_URL=ws://127.0.0.1:8080 IC_URL=http://127.0.0.1:4943 FETCH_IC_ROOT_KEY=true DFX_NETWORK=local TEST_CANISTER_ID=$CANISTER_ID_TEST_CANISTER npm test

echo "Stopping gateway"
kill $gateway_pid

echo "Stopping local replica"
dfx stop

#!/bin/bash

set -e

# requires running the prepare_integration_tests.sh script first

echo "Starting local replica..."
dfx start --clean --background

echo "Starting gateway in the background..."
cargo run --release > /dev/null 2>&1 &
gateway_pid=$!

echo "Waiting for gateway to start..."
sleep 1

echo "Deploying test canister..."
cd tests
dfx deploy test_canister_rs --no-wallet
# generate test_canister JS declarations
npm run generate:test_canister_rs
# source the canister id from the generated .env file
source .env

echo "Running integration test..."
WS_GATEWAY_URL=ws://127.0.0.1:8080 IC_URL=http://127.0.0.1:4943 FETCH_IC_ROOT_KEY=true DFX_NETWORK=local npm run integration:test

echo "Stopping gateway..."
kill $gateway_pid

echo "Stopping local replica..."
dfx stop

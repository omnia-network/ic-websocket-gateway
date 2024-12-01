#!/bin/bash

set -e

# requires running the prepare_tests.sh script first

echo "Starting local replica..."
dfx start --clean --background

echo "Starting gateway in the background..."
cargo run --release > /dev/null 2>&1 &
gateway_pid=$!

echo "Waiting for gateway to start..."
sleep 1

export DFX_NETWORK=local
export WS_GATEWAY_URL=ws://127.0.0.1:8080
export IC_URL=http://127.0.0.1:4943
export FETCH_IC_ROOT_KEY=true

echo "Deploying test canister (Rust)..."
cd tests
dfx deploy test_canister_rs --no-wallet
# generate test_canister JS declarations,
# which will be used for both Rust and Motoko tests
npm run generate:test_canister_rs

echo "Running integration test (Rust)..."
TEST_CANISTER_ID=$(dfx canister id test_canister_rs) npm run integration:test

echo "Deploying test canister (Motoko)..."
dfx deploy test_canister_mo --no-wallet

echo "Running integration test (Motoko)..."
TEST_CANISTER_ID=$(dfx canister id test_canister_mo) npm run integration:test

echo "Stopping gateway..."
kill $gateway_pid

echo "Stopping local replica..."
dfx stop

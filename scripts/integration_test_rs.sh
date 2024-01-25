#!/bin/bash

echo "Starting local replica"
dfx start --clean --background

echo "Starting gateway in the background"
RUST_LOG_STDOUT=ic_websocket_gateway=debug cargo run > scripts/gateway_test.log &
pid=$!

echo "Deploying Rust test canister"
cd tests
npm run generate:test_canister_rs
dfx deploy test_canister_rs --no-wallet

echo "Running integration test"
npm run integration:test

# kill gateway process
kill $pid

dfx stop

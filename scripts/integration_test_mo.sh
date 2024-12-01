#!/bin/bash

# requires running the prepare_tests.sh script first

echo "Starting local replica"
dfx start --clean --background

echo "Starting gateway in the background"
RUST_LOG_STDOUT=ic_websocket_gateway=debug cargo run > scripts/gateway_test.log &
pid=$!

echo "Deploying Motoko test canister"
cd tests
npx mops install
npm run generate:test_canister_mo
dfx deploy test_canister_mo --no-wallet

echo "Running integration test"
npm run integration:test

# kill gateway process
kill $pid

dfx stop

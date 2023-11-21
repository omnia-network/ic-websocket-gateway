#!/bin/bash

echo "Starting local replica"
dfx start --clean --background

echo "Building gateway"
cargo build

echo "Starting gateway in the background"
RUST_LOG=ic_websocket_gateway=debug cargo run > scripts/gateway_test.log &
pid=$!

echo "Obtaining gateway principal..."
GATEWAY_PRINCIPAL=$(cargo run -q -p scripts --bin principal_from_key_pair "./data/key_pair")
echo "Gateway principal: $GATEWAY_PRINCIPAL"

echo "Deploying Rust test canister"
cd tests
npm run generate:test_canister_rs
dfx deploy test_canister_rs --no-wallet --argument "(opt \"$GATEWAY_PRINCIPAL\")"

echo "Running integration test"
npm run integration:test

# kill gateway process
kill $pid

dfx stop

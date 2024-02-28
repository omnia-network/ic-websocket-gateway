#!/bin/bash

echo "Deploying Rust test canister"
cd tests
npm run generate:test_canister_rs
dfx deploy test_canister_rs --no-wallet

echo "Running integration test"
npm run integration:test

# kill gateway process
kill $pid

dfx stop

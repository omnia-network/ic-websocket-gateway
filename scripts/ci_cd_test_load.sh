#!/bin/bash

set -e

# requires running the prepare_tests.sh script first

echo "Starting local replica..."
dfx start --clean > /dev/null 2>&1 &

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

# Compile load test script
echo "Compiling load test script..."
npm run load:bundle

# Create reports directory if it doesn't exist
mkdir -p reports

# Run load tests
echo "Running load tests..."
LOG_LEVEL=error npx artillery run gateway_load_tests.yml --output reports/gateway_load_tests.json

echo "Results:"
cat reports/gateway_load_tests.json

# Check that no users have failed
echo -e "\nChecking that no users have failed..."
failed=$(jq '.aggregate.counters."vusers.failed"' reports/gateway_load_tests.json)
if [ "$failed" != 0 ]; then
    echo "ERROR: Load test failed with $failed users failing."
    exit 1
else
    echo "All users were successful."
fi

echo -e "\nStopping gateway..."
kill $gateway_pid

echo "Stopping local replica..."
dfx stop

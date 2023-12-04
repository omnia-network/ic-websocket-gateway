echo "Starting local replica"
dfx start --clean --background

echo "Building gateway"
cargo build

echo "Starting gateway in the background"
RUST_LOG_STDOUT=ic_websocket_gateway=debug cargo run > scripts/gateway_test.log &
pid=$!

echo "Deploying Motoko test canister"
cd tests
npm run generate:test_canister_mo
dfx deploy test_canister_mo --no-wallet

echo "Running integration test"
npm run integration:test

# kill gateway process
kill $pid

dfx stop

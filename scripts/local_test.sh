echo "Starting local replica"
dfx start --clean --background

echo "Running unit tests for gateway"
cargo test --workspace -- --test-threads=1

echo "Building gateway"
cargo build

echo "Starting gateway in the background"
RUST_LOG=ic_websocket_gateway=debug cargo run > scripts/gateway_test.log &
pid=$!

echo "Deploying test canister"
cd tests/test_canister_rs
dfx deploy

cd ../integration
echo "Running integration test"
npm test

# kill gateway process
kill $pid

dfx stop
echo "Starting local replica"
dfx start --clean --background

echo "Building gateway"
cargo build

echo "Starting gateway in the background"
RUST_LOG=ic_websocket_gateway=debug cargo run > scripts/gateway_test.log &
pid=$!

echo "Deploying test canister"
cd tests/test_canister_mo
dfx deploy

cd ../integration
echo "Running integration test"
npm run test

# kill gateway process
kill $pid

dfx stop
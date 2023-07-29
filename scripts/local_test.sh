echo "Starting local replica"
dfx start --clean --background

echo "Starting gateway in the background"
cargo run > scripts/gateway_test.log &
pid=$!

echo "Deploying test canister"
cd tests/integration
dfx deploy

echo "Running integration test"
python3 run_n_clients.py

# kill gateway process
kill $pid

dfx stop
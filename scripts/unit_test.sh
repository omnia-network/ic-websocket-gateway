#!/bin/bash

echo "Starting local replica"
dfx start --clean --background

echo "Running unit tests for gateway"
cargo test -p ic_websocket_gateway -- --test-threads=1
cargo test -p ic-identity

dfx stop

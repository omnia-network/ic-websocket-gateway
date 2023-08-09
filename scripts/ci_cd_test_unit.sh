#!/bin/bash

set -e

echo "Starting local replica..."
dfx start --clean --background

echo "Running unit tests..."
cargo test -p ic_websocket_gateway -- --test-threads=1
cargo test -p ic-identity

echo "Stopping local replica..."
dfx stop

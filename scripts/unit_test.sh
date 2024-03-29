#!/bin/bash

set -e

echo "Running unit tests..."
cargo test --release -p ic_websocket_gateway -- --test-threads=1
cargo test --release -p gateway-state
cargo test --release -p canister-utils
cargo test --release -p ic-identity

#!/bin/bash

echo "Starting local replica"
dfx start --clean --background

echo "Running unit tests for gateway"
cargo test --workspace -- --test-threads=1

dfx stop

#!/bin/bash

set -e

echo "Building gateway"
cargo build --release

cd tests

# install tests dependencies
npm install

# install Rust dependencies
rustup target add wasm32-unknown-unknown

#!/bin/bash

cd tests

# install integration tests dependencies
cd integration
npm install

# install test_canister dependencies
cd ../test_canister
rustup target add wasm32-unknown-unknown

npm install

# generate test_canister declarations
npm run generate

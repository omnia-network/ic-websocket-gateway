#!/bin/bash

cd tests

# install integration tests dependencies
cd integration
npm install

# install test_canister dependencies
cd ../test_canister_rs

rustup target add wasm32-unknown-unknown

npm install

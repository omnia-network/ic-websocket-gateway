#!/bin/bash

cd tests

# install tests dependencies
npm install

# install Rust dependencies
rustup target add wasm32-unknown-unknown

#!/bin/bash

cd tests

# install integration tests dependencies
cd integration
npm install

# install test_canister dependencies
cd ../test_canister
npm install

# generate test_canister declarations
npm run generate

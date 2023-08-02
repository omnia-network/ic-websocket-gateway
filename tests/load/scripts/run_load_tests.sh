#!/bin/bash

set -e

# Compile load test script
echo "Compiling load test script..."
npm run bundle

# Create reports directory if it doesn't exist
mkdir -p reports

# Run load tests
echo "Running load tests..."
LOG_LEVEL=debug npx artillery run gateway_load_tests.yml --output reports/gateway_load_tests.json

# Generate HTML report
echo "Generating HTML report..."
npx artillery report reports/gateway_load_tests.json -o reports/gateway_load_tests.html

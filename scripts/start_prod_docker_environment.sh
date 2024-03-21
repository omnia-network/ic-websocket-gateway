#!/bin/bash

echo "Creating the telemetry/prometheus/prometheus-prod.yml file"
# Load and export variables from .env file
set -a  # Automatically export all variables
source .env
set +a  # Stop automatically exporting

# Run envsubst
envsubst < ./telemetry/prometheus/prometheus-template.yml > ./telemetry/prometheus/prometheus-prod.yml
echo "File telemetry/prometheus/prometheus-prod.yml successfully created"

echo "Starting docker environment"
docker compose -f docker-compose.yml -f docker-compose-prod.yml --env-file .env up -d

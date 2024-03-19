#!/bin/bash

echo "Creating the telemetry/prometheus/prometheus-prod.yml file"
# Load and export variables from .env file
set -a  # Automatically export all variables
source .env
set +a  # Stop automatically exporting

# Run envsubst
envsubst < ./telemetry/prometheus/prometheus-template.yml > ./telemetry/prometheus/prometheus-prod.yml

# https://docs.docker.com/compose/environment-variables/set-environment-variables/#:~:text=doesnotexist/.env.dev-,You%20can%20use%20multiple,them%20in%20order.%20Later%20files%20can%20override%20variables%20from%20earlier%20files.,-%24%20docker%20compose
echo "Starting docker environment"
docker compose -f docker-compose.yml -f docker-compose-prod.yml --env-file .env up -d

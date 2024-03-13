#!/bin/bash

echo "Creating the telemetry/prometheus/prometheus-prod.yml file"
#source ../.env && envsubst < ../telemetry/prometheus/prometheus-template.yml > ../telemetry/prometheus/prometheus-prod.yml
#source ../.env && echo $GRAFANA_PROMETHEUS_ENDPOINT

# https://docs.docker.com/compose/environment-variables/set-environment-variables/#:~:text=doesnotexist/.env.dev-,You%20can%20use%20multiple,them%20in%20order.%20Later%20files%20can%20override%20variables%20from%20earlier%20files.,-%24%20docker%20compose
#echo "Starting docker environment"
#docker compose -f docker-compose.yml -f docker-compose-prod.yml --env-file .env up -d

#!/bin/bash

echo "Starting local replica"
dfx start --clean --background

echo "Removing previous local websocket image"
docker rmi local/ic-websocket-gateway

# https://docs.docker.com/compose/environment-variables/set-environment-variables/#:~:text=doesnotexist/.env.dev-,You%20can%20use%20multiple,them%20in%20order.%20Later%20files%20can%20override%20variables%20from%20earlier%20files.,-%24%20docker%20compose
echo "Starting docker environment"
docker compose -f docker-compose.yml -f docker-compose-local.yml --env-file .env.local up -d --build

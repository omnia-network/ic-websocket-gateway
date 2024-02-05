#!/bin/bash

echo "Stopping docker environment"
docker compose -f docker-compose.yml -f docker-compose-local.yml down

echo "Stopping local replica"
dfx stop

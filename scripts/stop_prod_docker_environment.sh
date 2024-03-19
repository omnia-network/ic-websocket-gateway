#!/bin/bash

echo "Stopping docker environment"
docker compose -f docker-compose.yml -f docker-compose-prod.yml down

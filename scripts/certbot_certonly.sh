#!/bin/bash

source .env

# obtain or renew a certificate by spinning up a temporary webserver on port 80
docker run -it --rm --name certbot -v $(pwd)/volumes/nginx/certs:/etc/letsencrypt -p 80:80 certbot/certbot certonly -d $DOMAIN_NAME --standalone

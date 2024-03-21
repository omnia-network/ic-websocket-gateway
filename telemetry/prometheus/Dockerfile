# Warning: This image is not intended to be pushed to a public repository because it contains sensitive information!!
# Important: This is an alternative solution to add the environment variables substitution feature to the official prom/prometheus image.
# Normally, we use the official image and we prepare the configuration file (prometheus.yml) with the environment variables substitution before running the container.
# This solution is useful when you have a private repository and you want to prepare 1 time the image and use it without
# the need to always prepare the configuration file

FROM prom/prometheus

COPY /telemetry/prometheus/prometheus-template.yml /etc/prometheus/prometheus.yml

WORKDIR /
COPY .env ./.env

RUN source ./.env && sed -i "s|\${GRAFANA_PROMETHEUS_ENDPOINT}|$GRAFANA_PROMETHEUS_ENDPOINT|g; s|\${GRAFANA_PROMETHEUS_USERNAME}|$GRAFANA_PROMETHEUS_USERNAME|g; s|\${GRAFANA_PROMETHEUS_PASSWORD}|$GRAFANA_PROMETHEUS_PASSWORD|g" /etc/prometheus/prometheus.yml

CMD ["--config.file=/etc/prometheus/prometheus.yml", "--storage.tsdb.path=/prometheus", "--web.console.libraries=/usr/share/prometheus/console_libraries", "--web.console.templates=/usr/share/prometheus/consoles"]

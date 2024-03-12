FROM prom/prometheus

COPY /telemetry/prometheus/prometheus.yml /etc/prometheus/prometheus.yml

WORKDIR /
COPY .env ./.env

RUN source ./.env && sed -i "s|\${GRAFANA_PROMETHEUS_ENDPOINT}|$GRAFANA_PROMETHEUS_ENDPOINT|g; s|\${GRAFANA_PROMETHEUS_USERNAME}|$GRAFANA_PROMETHEUS_USERNAME|g; s|\${GRAFANA_PROMETHEUS_PASSWORD}|$GRAFANA_PROMETHEUS_PASSWORD|g" /etc/prometheus/prometheus.yml

CMD ["--config.file=/etc/prometheus/prometheus.yml", "--storage.tsdb.path=/prometheus", "--web.console.libraries=/usr/share/prometheus/console_libraries", "--web.console.templates=/usr/share/prometheus/consoles"]

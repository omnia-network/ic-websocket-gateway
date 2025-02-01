### install packages
FROM rust:1.79-slim-bullseye AS deps
WORKDIR /ic-ws-gateway
# this takes a while due to crates index update, so we do it first
RUN cargo install cargo-chef

### prepare the build
FROM deps AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

### build the IC WS Gateway
FROM deps AS builder

RUN apt update
RUN apt install -y pkg-config libssl-dev

COPY --from=planner /ic-ws-gateway/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --recipe-path recipe.json
# Build application
COPY . .
RUN cargo build --release

### run the IC WS Gateway (we don't need the rust toolchain to run the binary)
FROM debian:bullseye-slim AS runtime
WORKDIR /ic-ws-gateway
# install some utils
RUN apt update
RUN apt install -y curl
# copy the compiled binary
COPY --from=builder /ic-ws-gateway/target/release/ic_websocket_gateway .

EXPOSE 8080
EXPOSE 9000

HEALTHCHECK --timeout=30s --interval=30s --retries=5 \
  CMD curl -s -i -N -X GET --head \
    -H "Connection: Upgrade" \
    -H "Upgrade: websocket" \
    -H "Sec-WebSocket-Version: 13" \
    -H "Sec-WebSocket-Key: jWPXCaHslL1epUzz0k29Qw==" \
    http://127.0.0.1:8080 \
    | grep -q "HTTP/1.1 101 Switching Protocols" && exit 0 || exit 1

# run the Gateway
ENTRYPOINT ["/ic-ws-gateway/ic_websocket_gateway"]

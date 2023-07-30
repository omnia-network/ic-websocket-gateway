### install packages
FROM rust:1.69-slim-bullseye AS deps
WORKDIR /ic-ws-gateway
# this takes a while due to crates index update, so we do it first
RUN cargo install cargo-chef

### prepare the build
FROM deps AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

### build the IC WS Gateway
FROM deps AS builder 
COPY --from=planner /ic-ws-gateway/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --recipe-path recipe.json
# Build application
COPY . .
RUN cargo build --release

### run the IC WS Gateway (we don't need the rust toolchain to run the binary)
FROM debian:bullseye-slim AS runtime
WORKDIR /ic-ws-gateway
# copy the compiled binary
COPY --from=builder /ic-ws-gateway/target/release/ic_websocket_gateway .

EXPOSE 8080

# run the Gateway
ENTRYPOINT ["/ic-ws-gateway/ic_websocket_gateway"]

# build: 'docker build . -t gateway:v0'
# run: 'docker run --rm -p 8080:8080 -v /Users/massimoalbarello/Documents/Omnia/ic-websocket/data:/gateway/data gateway:v0'

FROM rust:latest

WORKDIR /gateway

COPY . .

RUN cargo build

EXPOSE 8080

CMD ["./target/debug/ic_websocket_gateway"]
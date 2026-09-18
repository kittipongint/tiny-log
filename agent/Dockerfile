FROM rust:1.98-alpine AS builder

RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

FROM alpine:3.21

RUN apk add --no-cache ca-certificates \
 && addgroup -S -g 10002 tinylogagent \
 && adduser -S -D -H -u 10002 -G tinylogagent tinylogagent

WORKDIR /app

COPY --from=builder \
    /app/target/release/tiny-log-agent \
    /app/tiny-log-agent

USER tinylogagent

ENV TINY_LOG_SYSTEM_INTERVAL_SECS=60
ENV TINY_LOG_SERVICE_INTERVAL_SECS=120
ENV TINY_LOG_DOCKER=true
ENV TINY_LOG_DOCKER_SOCK=/var/run/docker.sock
ENV TINY_LOG_DISK_PATH=/
ENV RUST_LOG=info

ENTRYPOINT ["/app/tiny-log-agent"]

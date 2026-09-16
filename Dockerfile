FROM rust:1.98-alpine AS builder

RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY web ./web

RUN cargo build --release

FROM alpine:3.21

RUN apk add --no-cache ca-certificates wget \
 && addgroup -S -g 10001 tinylog \
 && adduser -S -D -H -u 10001 -G tinylog tinylog

WORKDIR /app

COPY --from=builder \
    /app/target/release/tiny-log \
    /app/tiny-log

COPY web /app/web

RUN mkdir -p /data \
    && chown -R tinylog:tinylog /data /app

USER tinylog

ENV TINY_LOG_HOST=0.0.0.0
ENV TINY_LOG_PORT=8080
ENV TINY_LOG_DATABASE=/data/logs.db
ENV TINY_LOG_WEB_DIR=/app/web

EXPOSE 8080

VOLUME ["/data"]

ENTRYPOINT ["/app/tiny-log"]
CMD ["serve"]

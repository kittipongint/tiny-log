FROM rust:1.98-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY web ./web

RUN cargo build --release

FROM debian:bookworm-slim

RUN useradd \
    --system \
    --uid 10001 \
    --create-home \
    tinylog \
 && apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates wget \
 && rm -rf /var/lib/apt/lists/*

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


FROM rust:1.88-bookworm AS builder

WORKDIR /app

ARG CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://rsproxy.cn/index/
ENV CARGO_REGISTRIES_CRATES_IO_INDEX=${CARGO_REGISTRIES_CRATES_IO_INDEX} \
    CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
    CARGO_NET_RETRY=10 \
    CARGO_HTTP_TIMEOUT=600 \
    CARGO_HTTP_MULTIPLEXING=false

# Cache dependency compilation separately from application source changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && printf 'fn main() {}\n' > src/main.rs
RUN cargo build --release

RUN rm -rf src
COPY src ./src
COPY migrations ./migrations
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/rust-app /usr/local/bin/rust-app
COPY --from=builder /app/migrations ./migrations

RUN mkdir -p logs src/assets/avatar src/assets/image src/assets/moment src/assets/video src/assets/music

ENV PORT=8100 \
    LOG_FILE=/app/logs/rust-app.log \
    FFMPEG_PATH=ffmpeg

EXPOSE 8100

CMD ["rust-app"]

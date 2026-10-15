FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates ffmpeg && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/target/release/anime-stream /usr/local/bin/anime-stream
COPY static ./static
ENV ANIME_SERVER__HOST=0.0.0.0
ENV ANIME_DATA__DIR=/data
EXPOSE 8080
VOLUME ["/data", "/library"]
ENTRYPOINT ["anime-stream"]

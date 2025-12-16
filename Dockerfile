FROM rust:latest AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY hikari-dance/frames_cache.bin ./hikari-dance/frames_cache.bin
RUN cargo build --release

FROM ubuntu:24.04
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/portfolio-v2 /usr/local/bin/portfolio-v2
COPY --from=builder /app/hikari-dance ./hikari-dance
ENV SECRETS_LOCATION=/run/secret/authorized_keys/id_ed25519
EXPOSE 22
CMD ["portfolio-v2", "--server"]

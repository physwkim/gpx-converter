# syntax=docker/dockerfile:1

# ---- Build stage ----
FROM rust:1-bookworm AS builder
WORKDIR /app

# Cargo.lock is committed, so --locked gives a reproducible build.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# ---- Runtime stage ----
FROM debian:bookworm-slim AS runtime

# Run as an unprivileged user (port 8080 is > 1024, no root needed).
RUN useradd --system --uid 10001 --no-create-home appuser

COPY --from=builder /app/target/release/gpx-converter /usr/local/bin/gpx-converter

USER appuser
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/gpx-converter"]

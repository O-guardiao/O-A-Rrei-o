# Build stage
FROM rust:1.82-slim-bookworm AS builder
WORKDIR /app
COPY . .
RUN apt-get update && apt-get install -y libsqlite3-dev pkg-config
RUN cargo build --release --bin arreio-cli

# Runtime stage
FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y libsqlite3-0 ca-certificates
COPY --from=builder /app/target/release/arreio-cli /usr/local/bin/arreio
COPY --from=builder /app/crates/arreio-gateway/assets /app/assets
EXPOSE 8080
ENTRYPOINT ["arreio"]
CMD ["serve"]

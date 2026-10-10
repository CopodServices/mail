FROM rust:1-slim-bookworm AS builder

WORKDIR /app

COPY . .

RUN cargo build --release --locked \
    --bin mail \
    --bin migrator

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/mail /usr/local/bin/mail
COPY --from=builder /app/target/release/migrator /usr/local/bin/migrator

EXPOSE 2525 2587 8080

ENTRYPOINT ["mail"]

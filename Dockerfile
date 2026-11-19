# syntax=docker/dockerfile:1

FROM rust:1.98-trixie AS build

# aws-lc-rs (the TLS backend) compiles C sources during the build.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Compile the dependencies against a stub main first. This layer only depends on the manifest
# and lockfile, so the CI layer cache reuses it until a dependency actually changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked

# Cargo decides what to rebuild by modification time, and the copied sources can be older than
# the stub build above; the touch makes sure the real main.rs is compiled.
COPY src ./src
RUN touch src/main.rs \
    && cargo build --release --locked \
    && cp target/release/social-bot /usr/local/bin/social-bot

FROM debian:trixie-slim

# Lets the deploy workflow prune this bot's superseded images without touching anything else
# on the host.
LABEL app=social-bot

# Root certificates for the TLS connections to Discord and the database.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home bot

COPY --from=build /usr/local/bin/social-bot /usr/local/bin/social-bot

USER bot
ENTRYPOINT ["social-bot"]

FROM alpine:3.21 AS build

# Install build dependencies
RUN apk add --update --no-cache \
		build-base \
		openssl-dev \
		rust cargo

WORKDIR /usr/src/app

# Copy source files
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Run unit tests
RUN cargo test

# Build release
RUN cargo build --release --locked

# Release target
FROM alpine:3.21

# Image metadata
LABEL org.opencontainers.image.source="https://github.com/balena-io-experimental/helios"
LABEL org.opencontainers.image.description="Balena's on device agent"
LABEL org.opencontainers.image.licenses=APACHE-2.0

# Install release dependencies
RUN apk add --update --no-cache \
		libstdc++ sqlite jq dbus

COPY start.sh /usr/bin
COPY --from=build /usr/src/app/target/release/helios /usr/bin

CMD ["/usr/bin/start.sh"]

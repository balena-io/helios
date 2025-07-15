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

# Build release
# Unit tests are run separately by CI
RUN cargo build --release --locked

# Release target
FROM alpine:3.21

WORKDIR /opt/helios

# Image metadata
LABEL org.opencontainers.image.source="https://github.com/balena-io-experimental/helios"
LABEL org.opencontainers.image.description="Balena's on device agent"
LABEL org.opencontainers.image.licenses=APACHE-2.0

# Install release dependencies
RUN apk add --update --no-cache \
		libstdc++ docker-cli jq dbus socat

COPY scripts /opt/helios
COPY --from=build /usr/src/app/target/release/helios /usr/bin

VOLUME /tmp/run
HEALTHCHECK --interval=5m --start-period=10s --timeout=30s --retries=3 \
	CMD echo -e "GET /v3/ping HTTP/1.1\r\nHost: localhost\r\n\r\n" | socat - UNIX-CONNECT:/tmp/run/helios.sock | grep -q "200 OK"
CMD ["/opt/helios/start.sh"]

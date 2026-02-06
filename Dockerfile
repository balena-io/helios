# Comma-separated list of features
ARG HELIOS_FEATURES=all

FROM alpine:3.23 AS build

ARG HELIOS_FEATURES

# Install build dependencies
RUN apk add --update --no-cache \
	build-base \
	clang clang-dev cmake \
	rust cargo

WORKDIR /usr/src/app

# Copy source files
COPY Cargo.toml Cargo.lock ./
COPY helios ./helios
COPY helios-api ./helios-api
COPY helios-legacy ./helios-legacy
COPY helios-oci ./helios-oci
COPY helios-remote ./helios-remote
COPY helios-remote-model ./helios-remote-model
COPY helios-state ./helios-state
COPY helios-util ./helios-util

# Build release
# Unit tests are run separately by CI
RUN cargo build --no-default-features --features $HELIOS_FEATURES --release --locked

# Release target
FROM alpine:3.23

WORKDIR /opt/helios

# Image metadata
LABEL org.opencontainers.image.source="https://github.com/balena-io/helios"
LABEL org.opencontainers.image.description="Balena's on device agent"
LABEL org.opencontainers.image.licenses=APACHE-2.0

# Install release dependencies
RUN apk add --update --no-cache \
	libstdc++ jq dbus socat

# Install docker v28 since docker v29 no longer supports 
# the balena-engine API 1.41
RUN apk add --no-cache \
  --repositories-file /dev/null \
  --repository https://dl-cdn.alpinelinux.org/alpine/v3.22/main \
  --repository https://dl-cdn.alpinelinux.org/alpine/v3.22/community \
  "docker-cli<29"

COPY scripts /opt/helios
COPY --from=build /usr/src/app/target/release/helios /usr/bin

VOLUME /tmp/run
VOLUME /cache/helios
VOLUME /local/helios
HEALTHCHECK --interval=5m --start-period=10s --timeout=30s --retries=3 \
	CMD echo -e "GET /ping HTTP/1.1\r\nHost: localhost\r\n\r\n" | socat - UNIX-CONNECT:/tmp/run/helios.sock | grep -q "200 OK"
CMD ["/opt/helios/start.sh"]

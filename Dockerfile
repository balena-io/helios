# Comma-separated list of features
ARG HELIOS_FEATURES=all
ARG HELIOS_BUILD=release

FROM alpine:3.23 AS build

ARG HELIOS_FEATURES
ARG HELIOS_BUILD

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
COPY helios-balenahup ./helios-balenahup
COPY helios-oci ./helios-oci
COPY helios-remote ./helios-remote
COPY helios-remote-model ./helios-remote-model
COPY helios-state ./helios-state
COPY helios-util ./helios-util
COPY helios-store ./helios-store

# Build release
# Unit tests are run separately by CI
RUN cargo build --no-default-features --features $HELIOS_FEATURES --locked $(test "$HELIOS_BUILD" = "release" && echo "--release")

# Release target
FROM alpine:3.23

ARG HELIOS_BUILD

WORKDIR /opt/helios

# Image metadata
LABEL org.opencontainers.image.source="https://github.com/balena-io/helios"
LABEL org.opencontainers.image.description="Balena's on device agent"
LABEL org.opencontainers.image.licenses=APACHE-2.0

# Install release dependencies
# socat is used for the healthcheck and also for exposing the
# socket to a local port
RUN apk add --update --no-cache socat libstdc++

COPY scripts /opt/helios
COPY --from=build /usr/src/app/target/$HELIOS_BUILD/helios /usr/bin

# busybox-style multicall: argv[0] == helios-legacy-takeover runs the migration
RUN ln -s helios /usr/bin/helios-legacy-takeover

VOLUME /config/helios
VOLUME /cache/helios
VOLUME /local/helios
HEALTHCHECK --interval=5m --start-period=10s --timeout=30s --retries=3 \
	CMD echo -e "GET /ping HTTP/1.1\r\nHost: localhost\r\n\r\n" | socat - UNIX-CONNECT:/tmp/run/helios.sock | grep -q "200 OK"
CMD ["/opt/helios/start.sh"]

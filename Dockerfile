FROM alpine:3.21 AS build

# Install build dependencies
RUN apk add --update --no-cache \
		build-base \
		rust cargo

WORKDIR /usr/src/app

# Copy source files
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Run unit tests
RUN cargo test

# Build release
RUN cargo build --release

# Release target
FROM alpine:3.21

# Install release dependencies
RUN apk add --update --no-cache \
		libgcc sqlite jq dbus

COPY start.sh /usr/bin
COPY --from=build /usr/src/app/target/release/theseus /usr/bin

CMD ["/usr/bin/start.sh"]

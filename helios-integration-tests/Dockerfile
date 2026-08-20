ARG ALPINE_VERSION=3.24

FROM alpine:${ALPINE_VERSION} AS build

# Install build dependencies and dbus
RUN apk add --update --no-cache \
		build-base \
		rust cargo \
		clang clang-dev cmake \
		dbus

WORKDIR /usr/src/app

# Copy workspace files
COPY helios-integration-tests ./

# Copy helios-util as a sub-folder on the integration workspace
COPY helios-util ./helios-util

FROM build AS build-mock-remote

RUN cargo build --release --bin mock-remote

# Mock balenaCloud API used by the tests
FROM alpine:${ALPINE_VERSION} AS mock-remote

RUN apk add --no-cache wget libstdc++

COPY --from=build-mock-remote /usr/src/app/target/release/mock-remote /usr/local/bin/mock-remote

CMD ["mock-remote"]

# Test runner (default target)
FROM build AS sut

# Run integration tests
CMD ["cargo", "test", "--", "--test-threads", "1"]

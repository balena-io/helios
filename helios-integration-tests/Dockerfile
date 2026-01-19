FROM alpine:3.22

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

# Run integration tests
CMD ["cargo", "test", "--", "--test-threads", "1"]

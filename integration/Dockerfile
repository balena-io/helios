FROM alpine:3.21

# Install build dependencies
RUN apk add --update --no-cache \
		build-base \
		openssl-dev \
		rust cargo

WORKDIR /usr/src/app

# Copy source files
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Run integration tests
CMD ["cargo", "test", "--", "--test-threads", "1"]

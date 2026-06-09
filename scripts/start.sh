#!/bin/sh

set -e

# Set to 1 to debug commands
DEBUG=${DEBUG:-0}
[ "${DEBUG}" = "1" ] && set -x

# Set some limits on service configuration
if [ -n "$HELIOS_REMOTE_POLL_INTERVAL_MS" ] && [ "$HELIOS_REMOTE_POLL_INTERVAL_MS" -lt 900000 ]; then
  HELIOS_REMOTE_POLL_INTERVAL_MS=900000
fi
unset HELIOS_REMOTE_MAX_POLL_JITTER_MS
# Do not allow min interval to be user configurable
unset HELIOS_REMOTE_MIN_INTERVAL_MS

[ -z "$DOCKER_HOST" ] && (
  echo "DOCKER_HOST is required" >&2
  exit 1
)

[ -z "$BALENA_DEVICE_UUID" ] && (
  echo "BALENA_DEVICE_UUID is required" >&2
  exit 1
)

# Check for Supervisor set variables and configure the local service, the variables come from these features
# - io.balena.features.supervisor-api: '1'
# - io.balena.features.balena-socket: '1'
# - io.balena.features.dbus: '1'
# - io.balena.features.balena-api: '1'

# Read configuration from BALENA_* variables
# Ignore any credentials env vars and use BALENA_DEVICE_UUID by default
HELIOS_UUID="${BALENA_DEVICE_UUID}"
export HELIOS_UUID
unset HELIOS_REMOTE_API_KEY

if [ -n "${BALENA_HOST_OS_VERSION}" ]; then
  HELIOS_HOST_OS_VERSION="${BALENA_HOST_OS_VERSION}"

  # Append the board revision to the OS version if available
  if [ -n "${BALENA_HOST_OS_BOARD_REV}" ]; then
    HELIOS_HOST_OS_VERSION="$HELIOS_HOST_OS_VERSION@${BALENA_HOST_OS_BOARD_REV}"
  fi

  export HELIOS_HOST_OS_VERSION
fi

# Do not allow the runtime dir to be configurable
unset HELIOS_HOST_RUNTIME_DIR

if [ -n "${BALENA_HOST_RUNTIME_DIR}" ]; then
  HELIOS_HOST_RUNTIME_DIR="${BALENA_HOST_RUNTIME_DIR}"
  unset BALENA_HOST_RUNTIME_DIR
  export HELIOS_HOST_RUNTIME_DIR
fi

# Do not allow the log display name to be user configurable
unset HELIOS_LOCAL_DISPLAY_NAME
if [ -n "${BALENA_SERVICE_NAME}" ]; then
  export HELIOS_LOCAL_DISPLAY_NAME="${BALENA_SERVICE_NAME}"
fi

# Run in unmanaged mode if the legacy Supervisor is unmanaged
if [ -n "${BALENA_API_URL}" ] && [ -n "${BALENA_API_KEY}" ]; then
  HELIOS_REMOTE_API_ENDPOINT="${BALENA_API_URL}"
  HELIOS_REMOTE_API_KEY="${BALENA_API_KEY}"
  export HELIOS_REMOTE_API_ENDPOINT
  export HELIOS_REMOTE_API_KEY
fi

# Set up the legacy Supervisor proxy and hand over via the takeover applet.
legacy_port=${HELIOS_LEGACY_PORT:-48480}
unset HELIOS_LEGACY_PORT
if [ -n "$BALENA_SUPERVISOR_API_KEY" ] && [ -n "$BALENA_SUPERVISOR_HOST" ] && [ -n "$BALENA_SUPERVISOR_PORT" ]; then
  export HELIOS_LEGACY_API_ENDPOINT="http://${BALENA_SUPERVISOR_HOST}:${legacy_port}"
  export HELIOS_LEGACY_API_KEY="${BALENA_SUPERVISOR_API_KEY}"

  # One-shot migration, busybox-style (argv[0] selects the applet).
  helios-legacy-takeover \
    --override-host "http://127.0.0.1:${BALENA_SUPERVISOR_PORT}" \
    --override-port "${legacy_port}"
fi

# Make variables available for the new process
export HELIOS_REMOTE_POLL_INTERVAL_MS

# Set XDG variables to directories on volumes
export XDG_CACHE_HOME=/cache
export XDG_CONFIG_HOME=/config
export XDG_STATE_HOME=/local
export XDG_RUNTIME_DIR=/tmp/run

# Remove the socket if it exists (we will need some proper handover at some point)
rm /tmp/run/helios.sock 2>/dev/null || true

# Start the new supervisor
exec helios --local-api-address /tmp/run/helios.sock

#!/bin/sh

set -e

# Set to 1 to debug commands
DEBUG=${DEBUG:-0}
[ "${DEBUG}" = "1" ] && set -x

# Set some limits on service configuration
if [ -n "$HELIOS_REMOTE_POLL_INTERVAL" ] && [ "$HELIOS_REMOTE_POLL_INTERVAL" -lt 900000 ]; then
  HELIOS_REMOTE_POLL_INTERVAL=900000
fi
unset HELIOS_REMOTE_POLL_MAX_JITTER
# Do not allow min interval to be user configurable
unset HELIOS_REMOTE_POLL_MIN_INTERVAL

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
HELIOS_DEVICE_UUID="${BALENA_DEVICE_UUID}"

# Run in unmanaged mode if the legacy Supervisor is unmanaged
if [ -n "${BALENA_API_URL}" ] && [ -n "${BALENA_API_KEY}" ]; then
  HELIOS_REMOTE_API_ENDPOINT="${BALENA_API_URL}"
  HELIOS_REMOTE_API_KEY="${BALENA_API_KEY}"
  export HELIOS_REMOTE_API_ENDPOINT
  export HELIOS_REMOTE_API_KEY
fi

# Setup the Supervisor
dir="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
. "$dir/setup-supervisor.sh"

# Make variables available for the new process
export HELIOS_REMOTE_POLL_INTERVAL

# Set XDG variables to directories on volumes
export XDG_CACHE_HOME=/cache

# Remove the socket if it exists (we will need some proper handover at some point)
rm /tmp/run/helios.sock || true

# Start the new supervisor
exec helios start --uuid "${HELIOS_DEVICE_UUID}" --local-api-address /tmp/run/helios.sock

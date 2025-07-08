#!/bin/sh

set -e

# Set to 1 to debug commands
DEBUG=${DEBUG:-0}
[ "${DEBUG}" = "1" ] && set -x

# Set some limits on service configuration
if [ -n "$HELIOS_REMOTE_POLL_INTERVAL_MS" ] && [ "$HELIOS_REMOTE_POLL_INTERVAL_MS" -lt 900000 ]; then
  HELIOS_REMOTE_POLL_INTERVAL=900000
fi
unset HELIOS_REMOTE_MAX_POLL_JITTER_MS
# Do not allow min interval to be user configurable
unset HELIOS_REMOTE_MIN_INTERVAL_MS

# Check for supervisor set variables, the script needs the following features enabled
# - io.balena.features.supervisor-api: '1'
# - io.balena.features.balena-socket: '1'
# - io.balena.features.dbus: '1'
# - io.balena.features.balena-api: '1'
for var in DOCKER_HOST BALENA_SUPERVISOR_HOST BALENA_SUPERVISOR_PORT BALENA_SUPERVISOR_ADDRESS BALENA_SUPERVISOR_API_KEY BALENA_DEVICE_UUID BALENA_API_KEY BALENA_API_URL; do
  eval val="\$$var"
  if [ -z "$val" ]; then
    echo "Error: variable '$var' is required" >&2
    exit 1
  fi
done

# Read credentials from BALENA_* variables
HELIOS_DEVICE_UUID="${BALENA_DEVICE_UUID}"
HELIOS_REMOTE_API_ENDPOINT="${BALENA_API_URL}"
HELIOS_REMOTE_API_KEY="${BALENA_API_KEY}"

# Load the supervisor takeover script
dir="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
. "$dir/supervisor-takeover.sh"

# Make variables available for the new process
export HELIOS_REMOTE_API_ENDPOINT
export HELIOS_REMOTE_API_KEY
export HELIOS_REMOTE_POLL_INTERVAL

# Start the new supervisor
exec helios --uuid "${HELIOS_DEVICE_UUID}" --fallback-address "http://${BALENA_SUPERVISOR_HOST}:$fallback_port" --fallback-api-key "${BALENA_SUPERVISOR_API_KEY}" --local-address 0.0.0.0

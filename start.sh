#!/bin/sh

set -e

# Set to 1 to debug commands
DEBUG=${DEBUG:-0}
[ "${DEBUG}" = "1" ] && set -x

# Supervisor fallback port
fallback_port=${FALLBACK_PORT:-48480}
unset FALLBACK_PORT

# Read credentials from config.json
DEVICE_UUID="$(jq -r .uuid /mnt/boot/config.json)"
HELIOS_REMOTE_API_ENDPOINT="$(jq -r .apiEndpoint /mnt/boot/config.json)"
HELIOS_REMOTE_API_KEY="$(jq -r .deviceApiKey /mnt/boot/config.json)"
HELIOS_REMOTE_POLL_INTERVAL="$(jq -r .appUpdatePollInterval /mnt/boot/config.json)"

# Set some limits on service configuration
if [ -n "$HELIOS_REMOTE_POLL_INTERVAL_MS" ] && [ "$HELIOS_REMOTE_POLL_INTERVAL_MS" -lt 900000 ]; then
  HELIOS_REMOTE_POLL_INTERVAL=900000
fi
unset HELIOS_REMOTE_MAX_POLL_JITTER_MS
unset HELIOS_REMOTE_MIN_INTERVAL_MS

# Check for required variables
for var in DOCKER_HOST BALENA_SUPERVISOR_HOST BALENA_SUPERVISOR_PORT BALENA_SUPERVISOR_ADDRESS BALENA_SUPERVISOR_API_KEY; do
  eval val="\$$var"
  if [ -z "$val" ]; then
    echo "Error: variable '$var' is not set" >&2
    exit 1
  fi
done

stop_supervisor() {
  dbus-send \
    --system \
    --print-reply \
    --dest=org.freedesktop.systemd1 \
    /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.StopUnit \
    string:"balena-supervisor.service" string:"replace"

  # Wait for the supervisor to stop listening
  while netstat -tuln | grep ":${BALENA_SUPERVISOR_PORT}" >/dev/null; do
    sleep 1
  done
}

restart_supervisor() {
  dbus-send --system \
    --dest=org.freedesktop.systemd1 \
    --type=method_call \
    --print-reply \
    /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.RestartUnit \
    string:"balena-supervisor.service" \
    string:"replace"
}

start_supervisor() {
  dbus-send --system \
    --dest=org.freedesktop.systemd1 \
    --type=method_call \
    --print-reply \
    /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.StartUnit \
    string:"balena-supervisor.service" \
    string:"replace"
}

# Legacy Supervisor database
legacy_db=/mnt/legacy/database.sqlite

enable_proxy() {
  # Override supervisor configuration
  sqlite3 "$legacy_db" "INSERT INTO config (key, value) VALUES ('apiEndpointOverride', '$BALENA_SUPERVISOR_ADDRESS') ON CONFLICT(key) DO UPDATE SET value=excluded.value;"
  sqlite3 "$legacy_db" "INSERT INTO config (key, value) VALUES ('listenPortOverride', '${fallback_port}') ON CONFLICT(key) DO UPDATE SET value=excluded.value;"
}

disable_proxy() {
  sqlite3 "$legacy_db" "DELETE FROM config WHERE key='apiEndpointOverride';"
  sqlite3 "$legacy_db" "DELETE FROM config WHERE key='listenPortOverride';"
}

setup_supervisor() {
  # If the port is already set, then skip initialization
  port=$(sqlite3 "$legacy_db" "SELECT value FROM config WHERE key='listenPortOverride'")
  [ "$port" = "$fallback_port" ] && return

  # Stop the old supervisor and enable the proxy
  stop_supervisor
  enable_proxy

  # Start the new supervisor in 10s
  (
    sleep 10
    start_supervisor
  ) &
}

if [ -n "$DISABLE" ]; then
  # If the DISABLE env var is set, disable the service and exit
  disable_proxy
  restart_supervisor
  exit
else
  setup_supervisor
fi

# Make variables available for the new process
export HELIOS_REMOTE_API_ENDPOINT
export HELIOS_REMOTE_API_KEY
export HELIOS_REMOTE_POLL_INTERVAL

# Start the new supervisor
exec helios --uuid "${DEVICE_UUID}" --fallback-address "http://${BALENA_SUPERVISOR_HOST}:$fallback_port" --fallback-api-key "${BALENA_SUPERVISOR_API_KEY}" --local-address 0.0.0.0

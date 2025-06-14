#!/bin/sh

set -e

# Set to 1 to debug commands
DEBUG=${DEBUG:-0}
[ "${DEBUG}" = "1" ] && set -x

# Supervisor override port
LEGACY_SUPERVISOR_PORT=${LEGACY_SUPERVISOR_PORT:-48480}

# Read credentials from config.json
BALENA_API_ENDPOINT="$(jq -r .apiEndpoint /mnt/boot/config.json)"

# Check for required variables
for var in DOCKER_HOST BALENA_SUPERVISOR_HOST BALENA_SUPERVISOR_PORT \
  BALENA_SUPERVISOR_API_KEY \
  BALENA_API_ENDPOINT; do
  eval val="\$$var"
  if [ -z "$val" ]; then
    echo "Error: variable '$var' is not set" >&2
    exit 1
  fi
done

setup_supervisor() {
  # Stop the old supervisor
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

  # Supervisor database
  db=/mnt/data/database.sqlite

  # Override supervisor configuration
  sqlite3 "${db}" "INSERT INTO config (key, value) VALUES ('apiEndpointOverride', 'http://localhost:${BALENA_SUPERVISOR_PORT}') ON CONFLICT(key) DO UPDATE SET value=excluded.value;"
  sqlite3 "${db}" "INSERT INTO config (key, value) VALUES ('listenPortOverride', '${LEGACY_SUPERVISOR_PORT}') ON CONFLICT(key) DO UPDATE SET value=excluded.value;"

  # Restart the supervisor
  (
    sleep 10
    dbus-send --system \
      --dest=org.freedesktop.systemd1 \
      --type=method_call \
      --print-reply \
      /org/freedesktop/systemd1 \
      org.freedesktop.systemd1.Manager.StartUnit \
      string:"balena-supervisor.service" \
      string:"replace"
  ) &
}

ready=$(sqlite3 /mnt/data/database.sqlite "SELECT COUNT(*) FROM config WHERE key='apiEndpointOverride'")
if [ "$ready" = "0" ]; then
  setup_supervisor
fi

BALENA_SUPERVISOR_ADDRESS="http://${BALENA_SUPERVISOR_HOST}:${LEGACY_SUPERVISOR_PORT}"
# Make variables available for the new process
export BALENA_API_ENDPOINT
export BALENA_SUPERVISOR_ADDRESS

# Start the proxy
exec next-balena-supervisor

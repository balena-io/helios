#!/bin/sh

set -e

# Set to 1 to debug commands
DEBUG=${DEBUG:-0}
[ "${DEBUG}" = "1" ] && set -x

# Supervisor override port
legacy_port=${LEGACY_SUPERVISOR_PORT:-48480}
unset LEGACY_SUPERVISOR_PORT

# Read credentials from config.json
BALENA_API_ENDPOINT="$(jq -r .apiEndpoint /mnt/boot/config.json)"

# Check for required variables
for var in DOCKER_HOST BALENA_SUPERVISOR_HOST BALENA_SUPERVISOR_PORT BALENA_SUPERVISOR_ADDRESS BALENA_API_ENDPOINT; do
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
  sqlite3 "$legacy_db" "INSERT INTO config (key, value) VALUES ('listenPortOverride', '${legacy_port}') ON CONFLICT(key) DO UPDATE SET value=excluded.value;"
}

disable_proxy() {
  sqlite3 "$legacy_db" "DELETE FROM config WHERE key='apiEndpointOverride';"
  sqlite3 "$legacy_db" "DELETE FROM config WHERE key='listenPortOverride';"
}

setup_supervisor() {
  # If the port is already set, then skip initialization
  port=$(sqlite3 "$legacy_db" "SELECT value FROM config WHERE key='listenPortOverride'")
  [ "$port" = "$legacy_port" ] && return

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

# Set the legacy supervisor address for the proxy
LEGACY_SUPERVISOR_ADDRESS="http://${BALENA_SUPERVISOR_HOST}:${legacy_port}"

# Make variables available for the new process
export BALENA_API_ENDPOINT
export LEGACY_SUPERVISOR_ADDRESS

# Start the new supervisor
exec theseus

#!/bin/sh

set -e

# Supervisor fallback port
fallback_port=${FALLBACK_PORT:-48480}
unset FALLBACK_PORT

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

setup_supervisor

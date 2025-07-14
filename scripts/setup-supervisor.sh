#!/bin/sh

set -e

# Supervisor fallback port
fallback_port=${HELIOS_FALLBACK_PORT:-48480}
unset HELIOS_FALLBACK_PORT

# Terminate early if supervisor variables are not set
[ -n "${BALENA_SUPERVISOR_API_KEY}" ] && [ -n "${BALENA_SUPERVISOR_HOST}" ] && [ -n "${BALENA_SUPERVISOR_PORT}" ] || return 0

stop_service() {
  svc=$1
  dbus-send \
    --system \
    --print-reply \
    --dest=org.freedesktop.systemd1 \
    /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.StopUnit \
    string:"$svc.service" string:"replace"

}

start_service() {
  svc=$1
  dbus-send --system \
    --dest=org.freedesktop.systemd1 \
    --type=method_call \
    --print-reply \
    /org/freedesktop/systemd1 \
    org.freedesktop.systemd1.Manager.StartUnit \
    string:"$svc.service" \
    string:"replace"
}

wait_for_port_release() {
  time=$1
  # If the supervisor port is being used, wait for a bit
  if netstat -tuln | grep ":${BALENA_SUPERVISOR_PORT}" >/dev/null; then
    sleep "$time"
  fi
}

find_docker_image() {
  docker inspect "$1" 2>/dev/null | jq -r '.[].Image'
}

find_db_dir() {
  img=$1
  dir=$2
  docker run --rm -i -v /mnt/data/resin-data:/data --entrypoint sh "$img" -c "test -f /data/$dir/database.sqlite" && echo "$dir"
}

# Get the supervisor image
supervisor_img=$(find_docker_image balena_supervisor || find_docker_image resin_supervisor)
[ -z "$supervisor_img" ] && {
  echo "balenaSupervisor image not found"
  exit 1
}

db_dir=$(find_db_dir "$supervisor_img" "balena-supervisor" || find_db_dir "$supervisor_img" "resin-supervisor")
[ -z "$db_dir" ] && {
  echo "balenaSupervisor database not found"
  exit 1
}

try_enable_proxy() {
  cat <<EOF | docker run --rm -i -v "/mnt/data/resin-data/$db_dir:/data" --entrypoint node "$supervisor_img" -
import sqlite3 from 'sqlite3';
const db = new sqlite3.Database('/data/database.sqlite');

const query = (s) =>
  new Promise((resolve, reject) =>
    db.all(s, (err, rows) => {
      if (err) {
        reject(err);
      } else {
        resolve(rows);
      }
    }),
  );

const rows = await query(
  "SELECT value FROM config where key='listenPortOverride'",
);
if (rows.length > 0) {
  const { value } = rows[0];
  if (value === '$fallback_port') {
    console.log('false');
    process.exit(0);
  }
}

await query(
  "INSERT INTO config (key, value) VALUES ('apiEndpointOverride', '$BALENA_SUPERVISOR_ADDRESS') ON CONFLICT(key) DO UPDATE SET value=excluded.value",
);
await query(
  "INSERT INTO config (key, value) VALUES ('listenPortOverride', '${fallback_port}') ON CONFLICT(key) DO UPDATE SET value=excluded.value",
);
console.log('true');
EOF
}

# Set-up helios fallback variables
HELIOS_FALLBACK_ADDRESS="http://${BALENA_SUPERVISOR_HOST}:$fallback_port"
HELIOS_FALLBACK_API_KEY="${BALENA_SUPERVISOR_API_KEY}"
export HELIOS_FALLBACK_ADDRESS
export HELIOS_FALLBACK_API_KEY

setup_supervisor() {
  # Configure the supervisor overrides
  res=$(try_enable_proxy)

  # If the port is already set, then skip initialization
  [ "$res" = "false" ] && return

  stop_service "balena-supervisor" || stop_service "resin-supervisor" || true

  # Wait 10 seconds for port release
  wait_for_port_release 10

  # Start the new supervisor in 10s
  (
    sleep 10
    start_service "balena-supervisor" || start_service "resin-supervisor" || true
  ) &
}

setup_supervisor

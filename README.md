# helios

[Balena's](https://www.balena.io/) on-device agent. The service is responsible for performing app updates, configuring the host system and generally monitoring and keeping the host and apps healthy.

Helios is designed to run in a diversity of environments and continue operating autonomously with little to no human input.

This project is an experimental replacement for the current [balenaSupervisor](https://github.com/balena-os/balenas-supervisor).

## Features (WIP)

- Self-configuring. When started, the service determines the capabilities of the host enviroment and available interfaces (e.g. container engine, D-Bus, OTA, etc.) and adapts its behavior to apply the remote target state within the given capabilities.
- Fault tolerant. The service can recover from failures and resume operations without human intervention.
- Self-healing. The service continuously monitors the system and perform corrective measures to ensure system health and keep the system on-target.
- Observable. The service state and its decisions can be determined from the system logs.

## Quickstart

You need the [balenaCLI](https://docs.balena.io/reference/balena-cli/latest/) for now.

Register a new device and note the UUID

```sh
balena device register <fleet-name> --deviceType generic-aarch64
# Registering to <fleet-name>: <uuid>
```

Get the device API key

```sh
balena config generate --device <uuid> --version 6.5.39+rev2 --appUpdatePollInterval 15
# deviceApiKey:          <api-key>
```

Run the service

```
cargo run -- --uuid <uuid> --remote-api-key <api-key> --remote-api-endpoint https://api.balena-cloud.com
```

For the full list of command line arguments use `cargo run -- --help`

## Running on a Balena device

The service can be set-up to take over the existing balena Supervisor on a running device. The service does this by becoming a proxy betweeen the existing supervisor and the Balena API and local applications. For more information see [balena-os/balena-supervisor#2422](https://github.com/balena-os/balena-supervisor/pull/2422).

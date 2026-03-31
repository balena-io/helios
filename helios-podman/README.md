# helios-podman

Helios support and tools specific for Podman. Wraps `helios-oci` to add
Podman-specific behavior: quadlet file generation and version-aware feature
downgrading.

## Migrated code from podlet

The quadlet type system and serializer under `src/podlet/` are adapted from
[podlet](https://github.com/containers/podlet/) (MPL-2.0), with CLI and
compose-file functionality stripped out. This subtree is kept separate so it
can be replaced by an upstream library crate once podlet publishes one.

| Field           | Value                                      |
| --------------- | ------------------------------------------ |
| Upstream repo   | <https://github.com/containers/podlet/>    |
| Upstream commit | `e40f8c932d6b81c6d32f4a9ccf89aa5ba2a917cb` |
| Upstream tag    | `v0.3.1` (closest release)                 |
| License         | MPL-2.0 (header on every migrated file)    |

### What was kept

All files under `src/podlet/` originate from podlet. Each carries the MPL-2.0
header and an "Adapted from" comment.

| Podlet source                                           | helios-podman destination              |
| ------------------------------------------------------- | -------------------------------------- |
| `src/quadlet.rs`                                        | `src/podlet/quadlet/mod.rs`            |
| `src/quadlet/container.rs`                              | `src/podlet/quadlet/container.rs`      |
| `src/quadlet/container/{device,mount,rootfs,volume}.rs` | same relative path under `src/podlet/` |
| `src/quadlet/container/mount/{idmap,tmpfs,mode}.rs`     | same relative path under `src/podlet/` |
| `src/quadlet/network.rs`                                | `src/podlet/quadlet/network.rs`        |
| `src/quadlet/volume.rs`                                 | `src/podlet/quadlet/volume.rs`         |
| `src/quadlet/service.rs`                                | `src/podlet/quadlet/service.rs`        |
| `src/quadlet/install.rs`                                | `src/podlet/quadlet/install.rs`        |
| `src/quadlet/globals.rs`                                | `src/podlet/quadlet/globals.rs`        |
| `src/quadlet/unit.rs`                                   | `src/podlet/quadlet/unit.rs`           |
| `src/serde.rs`                                          | `src/podlet/serde/mod.rs`              |
| `src/serde/quadlet.rs`                                  | `src/podlet/serde/quadlet.rs`          |
| `src/serde/args.rs`                                     | `src/podlet/serde/args.rs`             |
| `src/serde/mount_options.rs`                            | `src/podlet/serde/mount_options/mod.rs` |
| `src/serde/mount_options/{ser,de}.rs`                   | same relative path under `src/podlet/` |
| `src/escape.rs`                                         | `src/podlet/escape.rs`                 |

### What was removed

- `Artifact`, `Pod`, `Kube`, `Build`, `Image` resource types and all related code
- `main.rs`, `cli.rs`, and the entire `cli/` directory
- `quadlet/{artifact,pod,kube,build,image}.rs`
- All `clap` derives and `#[arg(...)]` / `#[value(...)]` attributes
- All `compose_spec` conversion impls (`TryFrom<compose_spec::*>`) — note that
  `compose_spec` types are still used for dependency condition parsing in
  `unit.rs` and `labels.rs`
- All `color_eyre` usage (replaced with `thiserror`)

### What was added

Code outside `src/podlet/` is helios-specific and builds on top of the podlet
types.

| File                    | Purpose                                                                          |
| ----------------------- | -------------------------------------------------------------------------------- |
| `src/lib.rs`            | `Client`, `Container`, `Network`, `Volume` proxies wrapping `helios-oci` with quadlet lifecycle (create/install/start/stop/remove) |
| `src/quadlet/mod.rs`    | `from_container_config`, `from_volume_config`, `from_network_config` — converts `helios-oci` types to podlet quadlet `File`s |
| `src/quadlet/labels.rs` | Parses `com.docker.compose.depends_on` labels into systemd unit dependencies     |

### How to update from a newer podlet release

1. Clone or fetch the new podlet tag.
2. Diff the upstream files listed above against the commit recorded in this
   README. Focus on `src/quadlet/` and `src/serde/` in podlet (mapped to
   `src/podlet/quadlet/` and `src/podlet/serde/` here).
3. Apply relevant changes to the corresponding helios-podman files. Common
   things to look for:
   - New `PodmanVersion` variants and their `Downgrade` impls.
   - New fields on `Container`, `Network`, or `Volume`.
   - Changes to the serde serializers.
4. Skip anything related to `Artifact`, `Pod`, `Kube`, `Build`, `Image`,
   `clap`, or `color_eyre` — those dependencies are not used here.
5. Run `cargo clippy -p helios-podman` and `cargo test -p helios-podman`.
6. Update the commit hash and tag in the table above.

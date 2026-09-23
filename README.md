# collocate

Lightweight containers on a shared kernel: one `clone3()` per container, copy-on-write roots, per-container cgroups and network namespaces on a shared bridge.

## Layout

| Crate | Role |
|---|---|
| `collocate-core` | Spec, wire protocol, client, /proc parsing |
| `collocate-sys` | The unsafe syscall layer (clone3, mounts, caps, pidfd, epoll) |
| `collocate-store` | Node-local metadata and startup reconcile |
| `collocate-net` | IPAM, nftables ruleset rendering, network commands |
| `collocate-runtime` | Launch sequence, cgroups, exec |
| `collocate-init` | PID 1 inside each container |
| `collocated` | The daemon |
| `collocate` | CLI, plus `collocate-relay` |
| `collocate-compose` | Compose model, `up`/`down`/`plan`, docker-compose converter |
| `collocate-controller` | Autoscaling, rolling updates, rollback, self-healing |
| `collocate-image` | Docker and OCI archive import, registry pull, layer extraction, image config mapping, rock detection |
| `collocate-registry` | OCI Distribution client (pull only); no collocate dependencies, usable on its own |
| `collocate-pebble` | Minimal Pebble API client over its unix socket (checks, services, logs); no collocate dependencies |
| `collocate-cluster` | LXD node provisioning and fan-out status |

## CLI

The `collocate` CLI follows the [Canonical CLI guidelines](https://discourse.ubuntu.com/c/project/design-system/cli-guidelines/62) (as used by LXD and Juju): containers are the implicit object of top-level verbs (`run`, `list`, `stop`, `delete`, `exec`, `logs`, ...), while other resource types get noun-then-verb subcommands (`image list`, `secret set`, `load-balancer create`, `cluster status`). Common short aliases work everywhere (`ls`, `rm`, `ps`, `lb`).

- `--format {table,json}` (not `-o`) selects output shape; `--verbosity {quiet,brief,verbose,debug,trace}` or `-q`/`--verbose` controls narration; both are global.
- Structured data goes to stdout; narration and success confirmations ("Started web.") go to stderr.
- Destructive commands (`delete`, `down`, `image prune`, `image delete`, `secret delete`, `load-balancer delete`) ask for confirmation on a terminal, and refuse to run non-interactively unless `-y`/`--yes` is given — scripts should always pass `--yes` explicitly.
- `list`/`status`/`image list`/`load-balancer list` support `--columns`, `--no-headers`, `--no-truncate`, and print a specific message ("No containers found.") instead of an empty table.

## Images and rocks

Images come from a registry or from an archive:

    collocate image pull ghcr.io/canonical/charmed-postgresql:14.10-22.04_edge
    collocate image pull --username bot --password-stdin registry.example.com/team/app:1.2 < token
    collocate image import app.tar            # docker save or OCI archive (rockcraft pack output)

Pulls speak the OCI Distribution API with anonymous or basic-credential bearer tokens, so Docker Hub, ghcr.io and most private registries work. Multi-arch indexes resolve to the host architecture, layers may be uncompressed, gzip or zstd, and every blob is digest-checked. Pulling a tag whose digest is already local downloads nothing, and layers shared between images are fetched once. `collocate run --image NAME` pulls a missing image before starting it.

A [rock](https://documentation.ubuntu.com/rockcraft/) is detected at import or pull time, either from a `pebble` entrypoint or from a `pebble` binary in its layers. `image list`, `image show` and `status` report it as `rock`. Rocks get Pebble-native behaviour with no configuration:

- **Health**: unless the image or service defines a healthcheck, the container is probed through Pebble's API. It is unhealthy when any Pebble check is `down` or any service is in `backoff`/`error`. Compose can select a check level explicitly with `healthcheck: {pebble: ready}` (`alive`, `ready` or `any`).
- **Logs**: `collocate logs` reads Pebble's service log buffer (`TIME [service] message`). `-s/--service` filters by service, and `--raw` shows the captured stdout/stderr instead. A stopped rock falls back to the captured output.
- **Exec**: `collocate exec -s SERVICE target -- cmd` runs the command through `pebble exec --context=SERVICE`, so it inherits that service's environment, user and working directory. `-e`, `-u`, `-w` and `--timeout` are forwarded to Pebble. Without `-s`, exec behaves exactly as for any other container.

The daemon reaches Pebble at `/proc/<pid>/root$PEBBLE_SOCKET`, falling back to `$PEBBLE/.pebble.socket` and then `/var/lib/pebble/default/.pebble.socket`. Nothing is published outside the container.

In compose, image services inherit the image's entrypoint, command, environment, user, working directory, stop signal and healthcheck, and the service's own fields override them. `pull_policy` follows docker-compose: `missing` (default), `always` (re-resolve the tag on every `up`, fetching only when its digest changed), or `never`. Private registries take credentials from the top-level `registries:` map, keyed by host, and the credentials may reference secrets:

    registries:
      ghcr.io:
        username: bot
        password: "${secrets.ghcr_token}"

## Packaging

    cargo install cargo-deb
    cargo build --release
    cargo deb -p collocated --target x86_64-unknown-linux-musl
    cargo deb -p collocate --target x86_64-unknown-linux-musl
    cargo deb -p collocate-controller --target x86_64-unknown-linux-musl

Produces `target/debian/{collocated,collocate,collocate-controller}_*.deb`. Each installs its systemd unit and, for `collocated`, the default config — but does not enable or start it; run `systemctl daemon-reload && systemctl enable --now collocated` (and `collocate-controller`) after installing.

## Testing

    cargo test --workspace                      # unprivileged; privileged tests skip
    ./scripts/priv-test.sh --workspace -- --test-threads=1   # runs them as root via sudo

The privileged tests start real containers, so they need root, cgroup v2, `nft`, `ip`, `nsenter` and a static `busybox`.

## Known limits

- The registry client pulls only: no push, no retry/backoff, no rate-limit handling, and no credential helpers.
- Rocks are consumed, not built: `rockcraft pack` integration is out of scope, so build with rockcraft and then `image import` or `image pull` the result.
- With `pull_policy: always`, `collocate-controller` re-resolves the tag on every reconcile tick (one manifest request per image per tick).
- Root filesystem is chosen automatically by `root_mode = "auto"`, in order: kernel overlayfs, then `fuse-overlayfs` (needs `/dev/fuse` and the `fuse-overlayfs` binary — works inside unprivileged LXD containers with no special host configuration), then a read-only bind mount as a last resort (single-layer images only). `collocate info`/`doctor` report which one is active.
- Networking uses the `ip` and `nft` command-line tools rather than netlink.
- Cluster support drives the `lxc` CLI and has not been exercised against a live LXD cluster.

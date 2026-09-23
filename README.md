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
| `collocate-image` | Docker archive import, layer extraction, image config mapping |
| `collocate-cluster` | LXD node provisioning and fan-out status |

## CLI

The `collocate` CLI follows the [Canonical CLI guidelines](https://discourse.ubuntu.com/c/project/design-system/cli-guidelines/62) (as used by LXD and Juju): containers are the implicit object of top-level verbs (`run`, `list`, `stop`, `delete`, `exec`, `logs`, ...), while other resource types get noun-then-verb subcommands (`image list`, `secret set`, `load-balancer create`, `cluster status`). Common short aliases work everywhere (`ls`, `rm`, `ps`, `lb`).

- `--format {table,json}` (not `-o`) selects output shape; `--verbosity {quiet,brief,verbose,debug,trace}` or `-q`/`--verbose` controls narration; both are global.
- Structured data goes to stdout; narration and success confirmations ("Started web.") go to stderr.
- Destructive commands (`delete`, `down`, `image prune`, `image delete`, `secret delete`, `load-balancer delete`) ask for confirmation on a terminal, and refuse to run non-interactively unless `-y`/`--yes` is given — scripts should always pass `--yes` explicitly.
- `list`/`status`/`image list`/`load-balancer list` support `--columns`, `--no-headers`, `--no-truncate`, and print a specific message ("No containers found.") instead of an empty table.

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

- No OCI registry client; images come from `collocate image import` (docker save archives).
- Root filesystem is chosen automatically by `root_mode = "auto"`, in order: kernel overlayfs, then `fuse-overlayfs` (needs `/dev/fuse` and the `fuse-overlayfs` binary — works inside unprivileged LXD containers with no special host configuration), then a read-only bind mount as a last resort (single-layer images only). `collocate info`/`doctor` report which one is active.
- Networking uses the `ip` and `nft` command-line tools rather than netlink.
- Cluster support drives the `lxc` CLI and has not been exercised against a live LXD cluster.

# collocate

Lightweight containers on a shared kernel: one `clone3()` per container, copy-on-write roots, per-container cgroups and network namespaces on a shared bridge.

## Install

collocate ships as a strictly confined snap that contains the CLI, the daemon (`collocate.daemon`), the controller (`collocate.controller`), the HTTPS gateway (`collocate.gateway`), the relay used across LXD nodes and the `nft`, `ip`, `fuse-overlayfs` and `lxc` tools it drives.

    sudo snap install collocate
    for p in docker-privileged network-control firewall-control fuse-support system-observe mount-observe process-control; do
        sudo snap connect collocate:$p
    done
    sudo snap connect collocate:lxd lxd:lxd              # only for the LXD mode
    sudo snap connect collocate:home-all                 # lets `sudo collocate` read files in other users' homes
    sudo groupadd --system collocate && sudo usermod -aG collocate $USER
    sudo collocate init

The daemon starts at install time but stays uninitialized: it only answers `info` and `init`, and creates no bridge, firewall table or cgroup until `collocate init` runs. The interfaces above are super-privileged and have to be connected by hand until the store grants auto-connection; `collocate init` and `collocate doctor` print the exact `snap connect` commands for anything missing.

Everything the snap owns lives under `/var/snap/collocate/common`: `collocated.toml`, `state/`, `run/collocate.sock` and `cluster.yaml`. The deb packages keep the FHS layout (`/etc/collocate`, `/var/lib/collocate`, `/run/collocate`). `snap remove collocate` kills every container and removes the bridge, the `inet collocate` nftables table and `collocate.slice`.

### `collocate init`

`collocate init` asks where containers should run:

- **local**: this machine runs containers. Init asks for the subnet, bridge, root filesystem mode, the group allowed on the socket and default limits, then the daemon rewrites its configuration and re-executes itself in place. Running containers survive; changing the subnet or bridge while containers or load balancers exist needs `--force`, which deletes them.
- **lxd**: containers run on LXD instances and this machine only drives them. Init asks for the LXD remote (the local LXD, or a URL and trust token), project, number of nodes (one per cluster member by default, placed with `--target`), image, per-node limits and how to install collocate on the nodes, then for every node: launch with the `collocate` profile (`security.nesting=true`), wait for it to be ready, install collocate, connect its interfaces, run `collocate init --preseed` inside it and check it answers through `collocate.relay`. Nodes are recorded in `cluster.yaml`, and re-running init resumes where it stopped.

Nodes install collocate from the store (`--install channel:latest/stable`, the default), from a local snap (`file:PATH`, installed with `--dangerous`) or from deb packages (`deb:collocated.deb,collocate.deb`).

Non-interactive forms:

    sudo collocate init --auto --subnet 10.40.0.0/16 --memory 512m
    sudo collocate init --auto --mode lxd --nodes 3 --node-memory 4g
    sudo collocate init --auto --mode lxd --node edge-1:lxd1 --node edge-2:lxd2 --install channel:latest/edge
    sudo collocate init --preseed < preseed.yaml
    collocate init --dump > preseed.yaml

A preseed carries the same answers:

    mode: lxd
    daemon:
      subnet: 172.30.0.0/16
      bridge: collocate0
      root_mode: auto
      group: collocate
      defaults: {memory: 512m, pids_max: 4096}
    cluster:
      remote: prod
      url: https://10.0.0.10:8443
      token: <trust token>
      project: default
      image: ubuntu:24.04
      install: channel:latest/stable
      nodes:
        edge-1: {target: lxd1, cpus: 2, memory: 4g}
        edge-2: {target: lxd2}

`cluster list`, `cluster status`, `cluster add-node`, `cluster remove-node` and `cluster up` read `cluster.yaml`. A compose file without `nodes:` spreads over the registered nodes; nodes it declares that are not registered yet are provisioned the same way init does.

### Confinement notes

- The CLI reads files through the `home` and `removable-media` interfaces: compose files, image archives, `--from-file` secrets and the packages `init` pushes to nodes must live in a home directory, `/media` or `/mnt`. Under `sudo`, reading another user's home (for example a `0750` `/home/you`) also needs `home-all` connected. Output redirected to a file outside those paths is dropped by AppArmor; pipe it instead.
- Bind volumes resolve inside the snap's mount namespace, where `/home`, `/mnt`, `/media`, `/run`, `/tmp` and `/var/snap` are the host's but `/srv` and `/opt` come from the base snap. Named volumes live in `/var/snap/collocate/common/state/volumes`.
- Image pulls and imports run in the daemon, so any member of the socket group can use them; `image import` passes the open archive to the daemon, which never needs to read your files.
- Workloads run under the daemon's AppArmor label, which the privileged `docker-support` interface makes permissive.
- The snap cannot load kernel modules. When `overlay` is not loaded, `root_mode = "auto"` uses the bundled `fuse-overlayfs`.
- The bundled `lxc` keeps its remotes in `/var/snap/collocate/common/lxc`, separate from `~/.config/lxc`; LXD mode against a remote therefore adds the remote itself from the URL and trust token, and commands against it need `sudo`.

## Remote access

collocate can be driven over the network by the `collocate` CLI on another machine or by any HTTPS client, authenticated with TLS client certificates, in the same way as LXD.

    sudo collocate init --https-address :8443          # or answer the "Address for remote clients" question
    sudo collocate trust add ci --role operator --projects web,api
    eyJjbGllbnRfbmFtZSI6ImNpIiwiZmluZ2VycHJpbnQiOi...   # the trust token

    collocate remote add prod <token>                   # on the other machine
    collocate remote switch prod                        # or --remote prod / COLLOCATE_REMOTE=prod per command
    collocate run -d --image nginx --project web --name web-1
    collocate exec web-1 -- nginx -T

`collocate.gateway` (`collocate-gateway.service` for the debs) terminates TLS 1.3 on `https_address` and forwards requests to the daemon on the local socket, tagged with the caller's identity; it runs with only the `network` and `network-bind` interfaces. The daemon authorizes every request, so local users of the socket are unaffected: the socket group keeps full access and `init` still needs root. The server certificate (ECDSA P-384, self-signed) lives in `state/trust/`, and `collocate info` shows its fingerprint.

**Trust.** A token is base64url JSON `{client_name, fingerprint, addresses, secret, expires_at, role, projects}`. `fingerprint` is the server certificate's SHA-256, so the client verifies the server before sending anything, and `addresses` lists every host address when `https_address` is a wildcard. Tokens are single use and never expire unless `--expiry 1h` (or `30m`, `7d`, ...) is given; only a hash of the secret is stored. Clients keep their certificate and remotes in `~/.config/collocate` (`$SNAP_USER_COMMON/config` in the snap, `COLLOCATE_CONFIG_DIR` to override).

| Command | Purpose |
|---|---|
| `collocate trust add NAME [--role viewer\|operator\|admin] [--projects a,b] [--expiry D]` | Print a trust token |
| `collocate trust add-certificate NAME client.crt [--role ...] [--projects ...]` | Trust a certificate directly |
| `collocate trust list \| show NAME \| remove NAME\|FINGERPRINT` | Manage trusted clients; removal cuts off open streams within 5 s |
| `collocate trust token list \| revoke NAME` | Manage pending tokens |
| `collocate remote add NAME TOKEN \| list \| remove \| rename \| switch \| get-default` | Client side |

**Roles.** `viewer` reads (`list`, `status`, `stats`, `logs`, `wait`, `info`, image and load-balancer listings, secret names). `operator` also runs containers, `exec`, compose, images (pull, import), secrets and load balancers. `admin` also manages trust, the controller and `init` (including over the network). A certificate restricted to projects only sees and touches containers, secrets, configs and load balancers of those projects, needs `--project` on `run`, and cannot delete or prune images.

**API.** Everything but the first two endpoints needs a trusted client certificate; errors come back as `{"kind":"error","data":{"code":N,"message":"..."}}` with a matching HTTP status (400, 403, 404, 409, 413, 429, 502, 503, 504).

| Endpoint | |
|---|---|
| `GET /1.0` | Server info: `auth` (`trusted`/`untrusted`), `caller`, `server_fingerprint`, `client_fingerprint` |
| `POST /1.0/certificates` | `{"token": "...", "name": "optional"}` enrolls the presented client certificate (201). Ten failures per minute per address are allowed. |
| `POST /1.0/call` | Body is any daemon request (`{"verb": "ps", "all": true, "project": null}`, `{"verb": "stop", "target": "web-1", "timeout_secs": 10}`, ...), reply is its response |
| `GET /1.0/exec` | WebSocket. First message: the `exec` request as JSON. Then binary messages prefixed by a channel byte: 0 stdin, 1 stdout, 2 stderr, 3 control (`{"eof":true}`), 4 exit (`{"status":N}` or `{"error":...,"code":N}`) |
| `POST /1.0/images` | Streamed docker or OCI archive (`Content-Length` or chunked) |
| `GET /1.0/logs?target=NAME[&follow=1][&tail=N][&service=a,b][&raw=1]` | Chunked log stream |

An external service needs nothing but a client certificate:

    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-384 -nodes -keyout svc.key -out svc.crt -days 3650 -subj /CN=svc
    curl -k --cert svc.crt --key svc.key -X POST -d '{"token":"<token>"}' https://host:8443/1.0/certificates
    curl -k --cert svc.crt --key svc.key -X POST -d '{"verb":"ps","all":true,"project":null}' https://host:8443/1.0/call

Pin the server rather than trusting `-k` blindly: the token's `fingerprint` is the SHA-256 of the server certificate (`openssl s_client -connect host:8443 | openssl x509 -noout -fingerprint -sha256`).

Compose files work remotely: rendered `configs` are sent to the daemon, which stores them under `state/configs/`, and bind-volume paths refer to the server's filesystem.

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
| `collocate-trust` | Certificates, fingerprints, trust tokens and the trust store |
| `collocate-remote` | TLS setup, HTTP framing, remotes and the HTTPS API client |
| `collocate-gateway` | HTTPS gateway for trusted remote clients |

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

The snap is built with snapcraft from `snap/snapcraft.yaml` (base `core26`, `amd64` and `arm64`):

    snapcraft pack                       # in an LXD or Multipass build instance
    sudo snapcraft pack --destructive-mode   # directly on an Ubuntu 26.04 host

Debs remain available:

    cargo install cargo-deb
    cargo build --release
    cargo deb -p collocated --target x86_64-unknown-linux-musl
    cargo deb -p collocate --target x86_64-unknown-linux-musl
    cargo deb -p collocate-controller --target x86_64-unknown-linux-musl

Produces `target/debian/{collocated,collocate,collocate-controller}_*.deb`. Each installs its systemd unit and, for `collocated`, the default config — but does not enable or start it; run `systemctl daemon-reload && systemctl enable --now collocated` (and `collocate-controller`) after installing. A deb install is initialized by its shipped config; `collocate init` rewrites the settings in `/etc/collocate/collocated.toml` and keeps any other keys there.

The controller reads `collocate-compose.yaml` next to the daemon config (`/etc/collocate` or `/var/snap/collocate/common/controller`) and waits until that file exists and the daemon is initialized. `collocate up --managed` deploys a file and hands it to the controller.

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
- Cluster support drives the `lxc` CLI. It has been exercised against a standalone LXD (provisioning, `cluster up`, `add-node`, `remove-node`) but not yet against a multi-member LXD cluster, where nodes are spread with `--target`.
- LXD nodes install the snap, which needs AppArmor inside the node; nested inside another container (LXD in LXD) snapd cannot start there, so use `--install deb:` in that setup.

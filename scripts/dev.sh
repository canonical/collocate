#!/bin/sh
set -e
. "$HOME/.cargo/env"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEV="${COLLOCATE_DEV_DIR:-$HOME/collocate-dev}"
BIN="$ROOT/target/x86_64-unknown-linux-musl/debug"

setup() {
    (cd "$ROOT" && cargo build --workspace)
    R="$DEV/state/images/noble/dev/rootfs"
    if [ ! -d "$R" ]; then
        mkdir -p "$R"/bin "$R"/etc "$R"/proc "$R"/sys "$R"/dev "$R"/run "$R"/tmp "$R"/root "$R"/.collocate
        for f in etc/resolv.conf etc/hosts etc/hostname .collocate/init; do : > "$R/$f"; done
        printf 'root:x:0:0:root:/root:/bin/sh\n' > "$R/etc/passwd"
        printf 'root:x:0:\n' > "$R/etc/group"
        cp /usr/bin/busybox "$R/bin/busybox"
        for a in $(/usr/bin/busybox --list); do [ "$a" = busybox ] || ln -sf busybox "$R/bin/$a"; done
        ln -sfn dev "$DEV/state/images/noble/latest"
    fi
    cat > "$DEV/collocated.toml" <<CFG
state_dir = "$DEV/state"
run_dir = "$DEV/run"
subnet = "172.30.0.0/16"
bridge = "colldev0"
root_mode = "auto"
init_path = "$BIN/collocate-init"
CFG
    mkdir -p "$DEV/run"
    echo "ready: $DEV"
}

daemon() {
    exec sudo "$BIN/collocated" --config "$DEV/collocated.toml"
}

cli() {
    sudo env COLLOCATE_HOST="$DEV/run/collocate.sock" COLLOCATE_STATE_DIR="$DEV/state" "$BIN/collocate" "$@"
}

clean() {
    sudo pkill -f "collocated --config $DEV" 2>/dev/null || true
    sleep 1
    if [ -d /sys/fs/cgroup/collocate.slice ]; then
        sudo sh -c 'echo 1 > /sys/fs/cgroup/collocate.slice/cgroup.kill' 2>/dev/null || true
        sleep 1
        sudo find /sys/fs/cgroup/collocate.slice -mindepth 2 -type d -delete 2>/dev/null || true
        sudo rmdir /sys/fs/cgroup/collocate.slice/containers /sys/fs/cgroup/collocate.slice/supervisor /sys/fs/cgroup/collocate.slice 2>/dev/null || true
    fi
    sudo ip link del colldev0 2>/dev/null || true
    sudo nft delete table inet collocate 2>/dev/null || true
    sudo rm -rf "$DEV"
}

case "$1" in
    setup) setup ;;
    daemon) daemon ;;
    cli) shift; cli "$@" ;;
    clean) clean ;;
    *) echo "usage: dev.sh setup|daemon|cli ARGS...|clean" >&2; exit 2 ;;
esac

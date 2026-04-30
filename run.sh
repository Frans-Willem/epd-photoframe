#!/usr/bin/env bash
# Flash + monitor helper.
#
# Usage: ./run.sh <e1001|e1002|e1004> <serial-port> <log-file>
# Example: ./run.sh e1002 /dev/ttyUSB1 /tmp/flash_e1002.log
#
# - Kills any still-running flash session for the same device
#   (tracked via /tmp/<device>_flash.pid) before starting a fresh one.
# - Writes the new PID to /tmp/<device>_flash.pid.
# - Runs under `script` so stdout goes both to the terminal and the
#   supplied log file (via tee-like behavior).
# - Blocks in the foreground for the whole flash+monitor session;
#   Ctrl-C ends it.

set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "usage: $0 <e1001|e1002|e1004> <serial-port> <log-file>" >&2
    exit 2
fi

device="$1"
port="$2"
log="$3"

if [[ "$device" != "e1001" && "$device" != "e1002" && "$device" != "e1004" ]]; then
    echo "error: device must be 'e1001', 'e1002', or 'e1004', got '$device'" >&2
    exit 2
fi

pid_file="/tmp/${device}_flash.pid"

# Kill any still-running session for this device. After `exec script
# …` below, the recorded PID belongs to `script`, which forwards
# signals to its child (the `cargo run` -> `espflash` chain) — so a
# plain SIGTERM to that PID is enough to bring the whole tree down.
if [[ -f "$pid_file" ]]; then
    old_pid=$(cat "$pid_file" 2>/dev/null || true)
    if [[ -n "${old_pid:-}" ]] && kill -0 "$old_pid" 2>/dev/null; then
        echo "killing previous flash session (pid $old_pid)" >&2
        kill -TERM "$old_pid" 2>/dev/null || true
        sleep 2
        kill -KILL "$old_pid" 2>/dev/null || true
    fi
    rm -f "$pid_file"
fi

# Source ESP env so the xtensa toolchain is on PATH.
# shellcheck disable=SC1090
source "$HOME/export-esp.sh"

# Record the PID and `exec` into `script`, so this shell process is
# replaced by `script` (PID stays the same) and the wrapper blocks in
# the foreground until `script` exits.
#
# `RELEASE=1` env var swaps in the release profile — needed when
# exercising release-only behaviour (e.g. the panic-to-screen path
# that's gated on `cfg(not(debug_assertions))`).
release_flag=""
if [[ "${RELEASE:-}" == "1" ]]; then
    release_flag="--release"
fi
echo $$ > "$pid_file"
exec script -qfc "cargo run $release_flag --features $device -- --port $port" "$log"

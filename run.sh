#!/usr/bin/env bash
# Flash + monitor helper.
#
# Usage: ./run.sh <e1002|e1004> <serial-port> <log-file>
# Example: ./run.sh e1002 /dev/ttyUSB1 /tmp/flash_e1002.log
#
# - Kills any still-running flash session for the same device
#   (tracked via /tmp/<device>_flash.pid) before starting a fresh one.
# - Writes the new PID to /tmp/<device>_flash.pid.
# - Runs under `script` so stdout goes both to the terminal and the
#   supplied log file (via tee-like behavior).
#
# On exit the PID file is left in place so a subsequent `./run.sh ...`
# invocation can spot and kill the previous session.

set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "usage: $0 <e1002|e1004> <serial-port> <log-file>" >&2
    exit 2
fi

device="$1"
port="$2"
log="$3"

if [[ "$device" != "e1002" && "$device" != "e1004" ]]; then
    echo "error: device must be 'e1002' or 'e1004', got '$device'" >&2
    exit 2
fi

pid_file="/tmp/${device}_flash.pid"

# Kill any still-running session for this device.
if [[ -f "$pid_file" ]]; then
    old_pid=$(cat "$pid_file" 2>/dev/null || true)
    if [[ -n "${old_pid:-}" ]] && kill -0 "$old_pid" 2>/dev/null; then
        echo "killing previous flash session (pid $old_pid)" >&2
        # Kill the whole process group so espflash/cargo/script all go down.
        kill -TERM "-$old_pid" 2>/dev/null || kill -TERM "$old_pid" 2>/dev/null || true
        # Give it a moment to release the serial port.
        sleep 2
        kill -KILL "-$old_pid" 2>/dev/null || kill -KILL "$old_pid" 2>/dev/null || true
    fi
    rm -f "$pid_file"
fi

# Source ESP env so the xtensa toolchain is on PATH.
# shellcheck disable=SC1090
source "$HOME/export-esp.sh"

# Run flash+monitor in its own process group so the kill above can
# target it wholesale. `setsid` gives us a new PGID that matches the
# leader PID; that's what we write to the PID file.
exec setsid bash -c "
    echo \$\$ > '$pid_file'
    exec script -qfc 'cargo run --features $device -- --port $port' '$log'
"

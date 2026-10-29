#!/bin/sh
# Usage: unproxy.sh UNIT SOCKET
# Stops the proxy running as systemd UNIT and moves SOCKET.real back to SOCKET.
set -e
unit=$1
socket=$2
systemctl stop "$unit" || true
if [ -S "$socket.real" ]; then
    rm -f "$socket"
    mv "$socket.real" "$socket"
fi
ls -la "$socket"

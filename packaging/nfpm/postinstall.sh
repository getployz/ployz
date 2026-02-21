#!/bin/sh
set -e

if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload || true
  systemctl enable ployzd.service ployz-runtime.service || true
  systemctl try-restart ployzd.service ployz-runtime.service || true
fi

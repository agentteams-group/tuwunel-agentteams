#!/bin/bash
set -euo pipefail

config="${TUWUNEL_CONFIG:-/etc/tuwunel/tuwunel.json}"
if [[ -f "$config" ]]; then
    exec tuwunel -c "$config" "$@"
fi
unset TUWUNEL_CONFIG
exec tuwunel "$@"

#!/usr/bin/env bash
set -euo pipefail

token_file="${VM_CONFIG_PATH:-/data/vm.cfg}"
mkdir -p "$(dirname "$token_file")"
touch "$token_file"

exec websockify \
    --token-plugin TokenFile \
    --token-source "$token_file" \
    6080 \
    --web /usr/share/novnc

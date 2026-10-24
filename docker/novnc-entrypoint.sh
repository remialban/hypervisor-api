#!/usr/bin/env bash
set -euo pipefail

token_file="${VM_CONFIG_PATH:-/vnc/vm.cfg}"

# L'API crée et met à jour ce fichier. Si le volume est en lecture seule, on ne peut
# pas le créer ici : websockify refusera simplement les connexions jusqu'à ce qu'il existe.
mkdir -p "$(dirname "$token_file")" 2>/dev/null || true
[ -e "$token_file" ] || touch "$token_file" 2>/dev/null || true

exec websockify \
    --token-plugin TokenFile \
    --token-source "$token_file" \
    6080 \
    --web /usr/share/novnc

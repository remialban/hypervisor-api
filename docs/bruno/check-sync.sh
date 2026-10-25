#!/usr/bin/env bash
# Compares the endpoints exposed by the running API (OpenAPI/Swagger) with the
# requests of this collection. Exit code 1 if they differ.
# Usage: ./check-sync.sh [openapi-url-or-file]   (default: http://127.0.0.1:3000/api-docs/openapi.json)
set -euo pipefail
cd "$(dirname "$0")"
SRC="${1:-http://127.0.0.1:3000/api-docs/openapi.json}"
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }

if [ -f "$SRC" ]; then SPEC=$(cat "$SRC"); else SPEC=$(curl -fsS "$SRC"); fi

norm() { sed -E 's/\{[^}]*\}/{}/g'; }

api=$(echo "$SPEC" | jq -r '.paths | to_entries[] | .key as $p | .value | keys[] | "\(ascii_upcase) \($p)"' | norm | sort -u)

col=$(grep -rhE '^(get|post|put|patch|delete) \{' --include='*.bru' -A1 . \
  | awk '/^(get|post|put|patch|delete) \{/ {m=toupper($1)} /url:/ {u=$2; print m " " u}' \
  | sed -E 's/\{\{baseUrl\}\}//; s/\{\{apiPrefix\}\}/\/api/; s/\{\{[A-Za-z]+Id\}\}/{}/g; s/\?.*$//' | norm | sort -u)

only_api=$(comm -23 <(echo "$api") <(echo "$col"))
only_col=$(comm -13 <(echo "$api") <(echo "$col"))

if [ -z "$only_api$only_col" ]; then echo "OK: collection and OpenAPI are in sync ($(echo "$api" | wc -l | tr -d ' ') endpoints)."; exit 0; fi
[ -n "$only_api" ] && { echo "In Swagger but missing from the collection:"; echo "$only_api" | sed 's/^/  + /'; }
[ -n "$only_col" ] && { echo "In the collection but no longer in Swagger:"; echo "$only_col" | sed 's/^/  - /'; }
exit 1

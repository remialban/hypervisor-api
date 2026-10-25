#!/usr/bin/env bash
# Copies the data of the old SQLite database (data/api.db) into PostgreSQL.
#
#   scripts/sqlite-to-postgres.sh [path/to/api.db]
#
# To run from the folder of docker-compose.yml, once the stack has started at least once
# on PostgreSQL (the API creates the tables at startup). It stops the API, EMPTIES every
# table of PostgreSQL, copies the SQLite data, then starts the API again. The SQLite file
# is only read.
#
# Needs sqlite3 on this machine. PSQL can be set to another psql command (tests).
set -euo pipefail

sqlite_db="${1:-data/api.db}"
[ -r "$sqlite_db" ] || { echo "Cannot read $sqlite_db" >&2; exit 1; }
command -v sqlite3 >/dev/null || { echo "sqlite3 is needed (package sqlite3)" >&2; exit 1; }

if [ -z "${PSQL:-}" ]; then
    set -a; [ -f .env ] && . ./.env; set +a
    PSQL="docker compose exec -T db psql -v ON_ERROR_STOP=1 -q -U ${POSTGRES_USER:-hypervisor} -d ${POSTGRES_DB:-hypervisor}"
    manage_api=1
else
    manage_api=0
fi

# Parents before children (foreign keys). vnc_tokens is not copied: they last 2 minutes.
tables="hypervisors users settings refresh_tokens hypervisor_access vm_owners vm_shares
audit_log user_groups user_group_members user_group_subgroups templates template_versions
template_permissions template_vms"

$PSQL -tAc "SELECT 1 FROM users LIMIT 0" </dev/null >/dev/null 2>&1 || {
    echo "The PostgreSQL tables do not exist: start the stack once (docker compose up -d)" >&2
    exit 1
}

read -r -p "Every table of PostgreSQL will be emptied, then filled from $sqlite_db. Continue? [y/N] " answer
[ "$answer" = "y" ] || [ "$answer" = "Y" ] || { echo "Cancelled."; exit 1; }

sqlite_ro="file:$(realpath "$sqlite_db")?immutable=1"
[ "$manage_api" = 1 ] && docker compose stop api >/dev/null

{
    echo "BEGIN;"
    echo "TRUNCATE $(echo $tables | tr ' ' ',') , vnc_tokens RESTART IDENTITY CASCADE;"
    for table in $tables; do
        sqlite_cols=$(sqlite3 -readonly "$sqlite_ro" "SELECT name FROM pragma_table_info('$table')")
        [ -n "$sqlite_cols" ] || continue
        pg_cols=$($PSQL -tAc "SELECT column_name FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = '$table'" </dev/null)
        cols=$(comm -12 <(echo "$sqlite_cols" | sort) <(echo "$pg_cols" | sort) | paste -sd, -)
        echo "COPY $table ($cols) FROM STDIN WITH (FORMAT csv);"
        # NULL is an unquoted empty field, '' is "" (the PostgreSQL CSV defaults). sqlite3 ends
        # rows with \r\n: the end marker must use the same line ending.
        sqlite3 -readonly -csv "$sqlite_ro" "SELECT $cols FROM $table"
        printf '\\.\r\n'
    done
    echo "SELECT setval(pg_get_serial_sequence('audit_log', 'id'), COALESCE(MAX(id), 0) + 1, false) FROM audit_log;"
    echo "COMMIT;"
} | $PSQL >/dev/null

[ "$manage_api" = 1 ] && docker compose start api >/dev/null
for table in $tables; do
    printf '%-22s %s\n' "$table" "$($PSQL -tAc "SELECT COUNT(*) FROM $table" </dev/null)"
done
echo "Done."

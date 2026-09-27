#!/bin/sh

. ./foreman/common.sh

# If foreman was killed hard enough to skip its own cleanup (e.g. SIGKILL or
# a closed terminal), an orphaned postmaster may still be holding our port.
# Stop it first, otherwise this postgres would die on the stale lock file and
# take the whole foreman stack down with it.
pg_ctl -D "$HYDRA_PG_SOCKET_DIR" -m fast stop >/dev/null 2>&1 || true

# initdb only has to run once; it fails noisily on an already-initialized
# data directory.
if [ ! -f "$HYDRA_PG_SOCKET_DIR/PG_VERSION" ]; then
    initdb "$HYDRA_PG_SOCKET_DIR"
fi

exec postgres -D "$HYDRA_PG_SOCKET_DIR" -k "$HYDRA_PG_SOCKET_DIR" -p "$HYDRA_PG_PORT"

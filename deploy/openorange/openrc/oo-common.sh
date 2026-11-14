# Sourced by the oo-* OpenRC units. Container lifecycle helpers.
#
# Each unit runs `docker run --rm` in the foreground under supervise-daemon.
# The docker CLI proxies SIGTERM to the container, so `rc-service X stop`
# is a graceful container stop; supervise-daemon respawns the CLI (and thus a
# fresh container) if it exits unexpectedly.
OO_HOME="${OO_HOME:-/opt/openorange}"
OO_BIN="$OO_HOME/bin/oo"
OO_LOGDIR=/var/log/openorange

oo_remove_container() {
	docker rm -f "$1" >/dev/null 2>&1 || true
}

# oo_wait SERVICE [APP] SECS -- block until the service answers
oo_wait() {
	"$OO_BIN" wait "$@"
}

# A failed readiness wait leaves supervise-daemon respawning the container
# although OpenRC reports "stopped"; stop the supervisor explicitly.
oo_fail_stop() {
	supervise-daemon "$RC_SVCNAME" --stop --pidfile "/run/supervise-$RC_SVCNAME.pid" >/dev/null 2>&1 || true
	oo_remove_container "$1"
}

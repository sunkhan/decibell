# Client hooks for scenarios.sh: the GPUI client (gpui-client/). A stub: fill each hook in when the client can be
# driven; the scenarios, the measurement and the table stay the ones the Electron baseline used (clients/electron.sh
# documents the contract). Sourced by scenarios.sh; $PERF is tools/perf.
#
# Same rules as the Electron baseline: release build, DECIBELL_CENTRAL=127.0.0.1:18080 (with the port) and a build that
# honours it (crates/decibell-core `local-central`), its own profile directory, never the live central; verify the
# central socket before typing a password (electron-local.sh shows how: a netns probe of the binary, then ss).

# GPUI_BIN="${GPUI_BIN:-$PERF/../../gpui-client/target/release/decibell}"
# GPUI_PROFILE="${DECIBELL_DEV_DIR:-$HOME/.cache/decibell-dev}/gpui-profile"

client_describe() { echo "GPUI client (not wired yet)"; }
client_preflight() { echo "gpui: clients/gpui.sh is a stub" >&2; return 1; }

# client_launch() {   # print the app's real pid: `setsid cmd & echo $!` is not it when setsid forks (it does when
#                      # the caller leads its process group); electron-local.sh's spawn() shows a way that is
#   ...                # DECIBELL_CENTRAL=127.0.0.1:18080 "$GPUI_BIN" (own profile dir), stdout/stderr to a log
# }
# client_ready()          { : wait for the main view (a stdout marker, or a control socket / --bench hook); }
# client_goto_home()      { : ; }
# client_goto_channel()   { : "open $2 in $1 without focusing the composer"; }
# client_minimize()       { : "minimize the window (xdg_toplevel.set_minimized) and stop rendering"; }
# client_stop()           { : "SIGTERM the pid, wait until it is gone"; }
# client_check()          { : "ss -tnp: connected to 127.0.0.1:18080, nothing to 178.104.131.247"; }
# client_measure_args()   { : ; }                      # one process: nothing extra
# client_startup_argv()   { printf '%s\0' env DECIBELL_CENTRAL=127.0.0.1:18080 "$GPUI_BIN" --profile "$GPUI_PROFILE"; }
# client_startup_args()   { printf '%s\0' --ready-marker 'first frame presented monotonic_ns=(?P<ns>\d+)'; }
# client_install_paths()  { printf 'gpui release\t%s\n' "$GPUI_BIN"; }
# client_scroll()         { : "its --bench-scroll SECS --bench-speed PX_S, printed as electron-scroll.mjs prints"; }
# client_screenshot()     { : ; }
# client_facts()          { python3 -B "$PERF/linux/scenario_util.py" display-server --pid "$1"; }

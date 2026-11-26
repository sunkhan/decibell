# Client hooks for scenarios.sh: the Electron client, built by electron-local.sh (isolated worktree, release addon with
# the local-central feature, electron-builder --linux dir) and run against the local test stack in the isolated
# profile ~/.cache/decibell-dev/electron-profile. Sourced by scenarios.sh; $PERF is tools/perf.
#
# The hook contract (every client file defines these; clients/gpui.sh is the stub to fill in):
#   client_describe            one line: what is being measured (version, commit, build)
#   client_preflight           fail (non-zero) when the client cannot run: not built, stack down, no saved sign-in
#   client_launch              start the client; print its root pid on the last stdout line
#   client_ready               wait until signed in and showing the main layout with the community connected
#   client_goto_home           show Home; nothing in the window keeps keyboard focus
#   client_goto_channel S C    open text channel C of server S without giving anything keyboard focus
#   client_minimize            minimize the window the way a user would (here: the title bar's button); the content
#                              must end up hidden (no frames), as a covered window is on Windows
#   client_stop                quit the client; no process may be left
#   client_check               (optional) assert the running client is safe (here: local central, no live socket)
#   client_measure_args        extra measure.sh arguments (NUL-separated)
#   client_startup_argv        the launch command for startup.sh (NUL-separated argv)
#   client_startup_args        extra startup.sh arguments (NUL-separated): --absent, --ready-marker, ...
#   client_startup_prepare     (optional) put the client in the state every startup run begins from
#   client_guard_start DIR / client_guard_stop   (optional) background safety watch while startup.sh runs it
#   client_install_paths       lines "label<TAB>path": what install-size measures
#   client_scroll SECS PX_S HZ the scroll benchmark on the open channel: one JSON line (electron-scroll.mjs format)
#   client_screenshot OUT      (optional) a PNG of the window's content
#   client_facts PID           (optional) one JSON object stored with each result (display server, ...)

EL="$PERF/electron-local.sh"
EL_DEV_DIR="${DECIBELL_DEV_DIR:-$HOME/.cache/decibell-dev}"
EL_USER_DATA="$EL_DEV_DIR/electron-profile/user-data"   # every Chromium process carries --user-data-dir=<this>
EL_STATE="$EL_DEV_DIR/electron-local"
EL_PORT="${DECIBELL_EB_CDP_PORT:-9333}"

_el_stamp() { sed -n "s/^$1=//p" "$EL_STATE/build.env" 2>/dev/null | tail -1; }

client_describe() {
  local commit mode ver
  commit="$(_el_stamp EB_COMMIT)"; mode="$(_el_stamp EB_MODE)"
  ver="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' \
        "$EL_DEV_DIR/electron-baseline/electron-client/package.json" 2>/dev/null || echo '?')"
  echo "Electron client $ver @ ${commit:0:10}, $mode (electron-builder --linux dir: asar + unpacked addon, as shipped);" \
       "addon release + local-central; Electron $(_el_stamp EB_ELECTRON); no Sentry DSN / GIF key baked;" \
       "isolated profile + private session bus with a throwaway keyring (--password-store=gnome-libsecret)"
}

client_preflight() {
  [[ -f "$EL_STATE/build.env" ]] || { echo "electron: not built: $EL build" >&2; return 1; }
  python3 -I -c 'import socket; socket.create_connection(("127.0.0.1",18080),timeout=2).close()' 2>/dev/null \
    || { echo "electron: the local stack is down: tools/dev/local-stack.sh up" >&2; return 1; }
  python3 -I -c 'import json,sys; c=json.load(open(sys.argv[1])); sys.exit(0 if c.get("saved_login") or c.get("credentials") else 1)' \
      "$EL_USER_DATA/config.json" 2>/dev/null \
    || { echo "electron: no saved sign-in: $EL run && $EL signin" >&2; return 1; }
  "$EL" probe >/dev/null
}

client_launch() { "$EL" run --cdp-port "$EL_PORT" | tail -1; }

client_ready() {
  "$EL" wait-main 90 >/dev/null
  # Daily Gaming is the community the scenarios use; wait until its tile is connected (not "Connecting…").
  node "$PERF/electron-cdp.mjs" "$EL_PORT" wait-server "Daily Gaming" 60 >/dev/null
  "$EL" dismiss-banner >/dev/null
  "$EL" check
}

client_goto_home() { "$EL" home; }

# open-channel.mjs clicks with element.click(), which gives nothing focus; the state line shows activeElement.
client_goto_channel() { "$EL" open "$1" "$2" && "$EL" state; }

client_minimize() { "$EL" minimize; }

client_stop() { "$EL" stop; }
client_check() { "$EL" check; }

# The root's tree plus anything else carrying this profile (a reparented crashpad handler would).
client_measure_args() { printf '%s\0' --also-match "user-data-dir=$EL_USER_DATA"; }

# The launch command exactly as electron-local.sh run builds it, without a CDP port (the installed app starts
# without one). `electron-local.sh cmd` also runs the probe and starts the private bus + keyring first.
client_startup_argv() {
  local cmd; cmd="$("$EL" cmd)" || return 1
  eval "local a=($cmd)"
  printf '%s\0' "${a[@]}"
}

client_startup_args() { printf '%s\0' --absent "user-data-dir=$EL_USER_DATA"; }

# Every startup run begins from: saved sign-in, Home showing when it last quit.
client_startup_prepare() {
  client_launch >/dev/null
  client_ready >/dev/null
  client_goto_home >/dev/null
  sleep 1
  client_stop
}

# While startup.sh launches the client (each run signs in with the saved sign-in), kill any instance that opens a
# socket to the live central. Polls every second; costs nothing inside the client.
client_guard_start() {
  local dir="$1"
  (
    while :; do
      if ss -tunapH 2>/dev/null | grep -qF "178.104.131.247"; then
        for p in /proc/[0-9]*; do
          { tr '\0' ' ' <"$p/cmdline"; } 2>/dev/null | grep -qF -- "user-data-dir=$EL_USER_DATA" \
            && kill -KILL "${p#/proc/}" 2>/dev/null
        done
        echo "$(date -Iseconds) live central socket seen: killed" >>"$dir/guard.log"
      fi
      sleep 1
    done
  ) &
  EL_GUARD=$!
}
client_guard_stop() { if [[ -n "${EL_GUARD:-}" ]]; then kill "$EL_GUARD" 2>/dev/null; wait "$EL_GUARD" 2>/dev/null || true; fi; }

client_install_paths() {
  printf 'installed /opt/Decibell (%s; measured, never run)\t/opt/Decibell\n' \
    "$(pacman -Q decibell-bin 2>/dev/null || echo 'not from pacman')"
  printf 'this build (%s)\t%s\n' "$(_el_stamp EB_MODE)" "$(dirname "$(_el_stamp EB_BIN)")"
}

client_scroll() { node "$PERF/electron-scroll.mjs" "$EL_PORT" "$1" "$2" "$3"; }

client_screenshot() { "$EL" screenshot "$1"; }

client_facts() {
  python3 -B "$PERF/linux/scenario_util.py" display-server --pid "$1" \
    --desktop-file /usr/share/applications/decibell.desktop
}

# ---- voice (the watch-1 / watch-2 scenarios): NOT IMPLEMENTED YET -----------------------------------------------------
# scenarios.sh's watch scenarios call these optional hooks (clients/gpui.sh has them); without them they refuse to run.
# What the Electron side needs, precisely:
#   client_voice_profile   MLS voice needs an account with E2EE keys: perf3 (passphrase decibell-m3-test-passphrase).
#                          electron-local.sh keeps ONE isolated profile ($DECIBELL_DEV_DIR/electron-profile, perf1):
#                          it needs a profile option (e.g. EL_PROFILE_NAME=electron-profile-voice → its own user-data
#                          and config.json) and `electron-local.sh signin --user perf3` into it once. Never the real
#                          profile. perf3 can't be in voice from both clients at once (one uid, one voice session):
#                          measure the two clients one after the other against the same streambot streams.
#   client_voice_join S C  over CDP (electron-cdp.mjs): if the E2EE prompt is up (PassphraseModal), type the passphrase
#                          into its input and press its submit button (or open it from Settings → Privacy); then set
#                          mute + deafen BEFORE joining (`invoke("set_voice_mute", {muted:true})`,
#                          `invoke("set_voice_deafen", {deafened:true})` through window.decibell.invoke) and click the
#                          voice channel's row in the channel list (the row whose text is C under the server S).
#   client_watch UID       click the stream tile (role=button, aria-label "<name>'s screen") or call the renderer's
#                          streamActions.focusStream through a window hook if one is exposed; watch_stream_too likewise.
#   client_video_stats UID the player's counters: useStreamStatsStore.getState().statsByUser[UID] (the renderer only
#                          publishes rates every 500 ms, not cumulative counts: record inputFps / fps / dropped).

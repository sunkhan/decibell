#!/usr/bin/env bash
# ship-isolated.sh: starts a ship binary with no saved sign-in, on a private D-Bus session with a
# throwaway gnome-keyring (never the session's keyring, never the owner's profile), for 10 s, and
# reports: its first frame, every socket / connection / name lookup it made (netlog.so), and what it
# put in the keyring. Then closes it.
#
#   packaging/tests/ship-isolated.sh <decibell> [seconds]
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="$(realpath "$1")"
secs="${2:-10}"
dir="$(mktemp -d "${TMPDIR:-/tmp}/decibell-ship-XXXXXX")"
cleanup() {
  [ -n "${app:-}" ] && kill "$app" 2>/dev/null || true
  [ -n "${keyring:-}" ] && kill "$keyring" 2>/dev/null || true
  [ -n "${bus_pid:-}" ] && kill "$bus_pid" 2>/dev/null || true
  rm -rf "$dir"
}
trap cleanup EXIT
gcc -shared -fPIC -O2 -o "$dir/netlog.so" "$here/netlog.c" -ldl

cat > "$dir/bus.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:path=$dir/bus</listen>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
read -r address bus_pid < <(dbus-daemon --config-file="$dir/bus.conf" --fork --print-address=1 --print-pid=1 | paste -sd' ')
mkdir -p "$dir/run" "$dir/home" && chmod 700 "$dir/run"
echo -n throwaway | env -i PATH="$PATH" HOME="$dir/home" XDG_DATA_HOME="$dir/home/data" XDG_RUNTIME_DIR="$dir/run" \
  DBUS_SESSION_BUS_ADDRESS="$address" gnome-keyring-daemon --foreground --components=secrets --unlock >/dev/null 2>&1 &
keyring=$!
for _ in $(seq 50); do
  DBUS_SESSION_BUS_ADDRESS="$address" secret-tool search --all nothing here >/dev/null 2>&1 && break
  if ! DBUS_SESSION_BUS_ADDRESS="$address" secret-tool search --all nothing here 2>&1 | grep -q .; then break; fi
  sleep 0.1
done

env -u DECIBELL_CENTRAL DBUS_SESSION_BUS_ADDRESS="$address" DECIBELL_USER_DATA_DIR="$dir/profile" \
  NETLOG_FILE="$dir/net.log" LD_PRELOAD="$dir/netlog.so" DECIBELL_LOG=info "$bin" --perf >"$dir/out.log" 2>&1 &
app=$!
sleep "$secs"
kill "$app"; wait "$app" 2>/dev/null || true
app=""

echo "== first frame / log"
grep -E 'first-frame|ERROR|WARN|\[update\]|\[sentry\]|\[e2ee\]' "$dir/out.log" | head -20 || true
echo "== sockets and connections (netlog)"
sort "$dir/net.log" | uniq -c | sort -rn | sed 's/^/  /' | head -30
inet="$(grep -E '^(socket (inet|inet6)|connect [0-9]+ inet|sendto|sendmsg [0-9]+ inet|getaddrinfo)' "$dir/net.log" || true)"
if [ -n "$inet" ]; then echo "NETWORK: the binary reached for the network:"; echo "$inet"; else echo "NETWORK: none (no inet socket, no connection, no lookup)"; fi
echo "== keyring (private, throwaway)"
items="$(DBUS_SESSION_BUS_ADDRESS="$address" secret-tool search --all username decibell 2>&1 | grep -E '^(label|attribute.url)' || true)"
if [ -n "$items" ]; then echo "$items" | sed "s|$dir|<tmp>|"; else echo "  (no item)"; fi
echo "== profile after the run"
find "$dir/profile" -maxdepth 2 | sed "s|$dir|<tmp>|" | sort

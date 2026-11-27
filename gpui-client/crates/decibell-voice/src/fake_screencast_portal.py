"""A stand-in for xdg-desktop-portal's ScreenCast interface, for the Go Live self-tests only.

It owns `org.freedesktop.portal.Desktop` on a *private* session bus the test made (never the
desktop's), answers the requests the core's Linux capture makes (`crates/decibell-core/src/media/
capture_pipewire.rs`: CreateSession, SelectSources, Start, OpenPipeWireRemote) and hands over a
PipeWire node the test created itself (a GStreamer test pattern): no picker, nothing of the
desktop is captured.

    python3 -I fake_screencast_portal.py --bus <address> --node <id> --control <dir>

Control files in <dir>: `refuse` (read at each SelectSources) makes it answer 1 (what a cancelled
picker answers); `close` (looked for every 100 ms) makes the portal close the newest started
session (what the desktop's "stop sharing" does). Every call is printed as one line.
"""

import argparse
import os
import socket
import sys

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib

SCREENCAST = "org.freedesktop.portal.ScreenCast"
REQUEST = "org.freedesktop.portal.Request"
SESSION = "org.freedesktop.portal.Session"
PROPS = "org.freedesktop.DBus.Properties"


def say(*words):
    print("PORTAL", *words, flush=True)


def sender_path(sender):
    return sender[1:].replace(".", "_")


class Request(dbus.service.Object):
    @dbus.service.signal(REQUEST, signature="ua{sv}")
    def Response(self, code, results):
        pass

    @dbus.service.method(REQUEST)
    def Close(self):
        self.remove_from_connection()


class Session(dbus.service.Object):
    def __init__(self, bus, path):
        super().__init__(bus, path)
        self.path = path
        self.open = True
        self.started = False

    def close_by_desktop(self):
        say("session-closed-by-desktop", self.path)
        self.open = False
        self.Closed({})

    @dbus.service.signal(SESSION, signature="a{sv}")
    def Closed(self, details):
        pass

    @dbus.service.method(SESSION)
    def Close(self):
        say("session-close", self.path)
        self.open = False
        self.remove_from_connection()


class Portal(dbus.service.Object):
    def __init__(self, bus, node, width, height, control):
        super().__init__(bus, "/org/freedesktop/portal/desktop")
        self.bus = bus
        self.node = node
        self.size = (width, height)
        self.control = control
        self.sessions = []
        GLib.timeout_add(100, self.watch)

    def watch(self):
        # `close`: the desktop ends the newest started session (what its "stop sharing" does).
        flag = os.path.join(self.control, "close")
        if os.path.exists(flag):
            live = [s for s in self.sessions if s.open and s.started]
            if live:
                os.remove(flag)
                live[-1].close_by_desktop()
        return True

    def session(self, path):
        return next((s for s in self.sessions if s.path == str(path)), None)

    def answer(self, sender, options, code, results):
        token = str(options.get("handle_token", "t"))
        path = f"/org/freedesktop/portal/desktop/request/{sender_path(sender)}/{token}"
        request = Request(self.bus, path)

        def respond():
            request.Response(dbus.UInt32(code), dbus.Dictionary(results, signature="sv"))
            request.remove_from_connection()
            return False

        # The caller subscribed to the Response before calling; answer after the reply.
        GLib.timeout_add(20, respond)
        return dbus.ObjectPath(path)

    @dbus.service.method(SCREENCAST, in_signature="a{sv}", out_signature="o", sender_keyword="sender")
    def CreateSession(self, options, sender=None):
        token = str(options.get("session_handle_token", "s"))
        path = f"/org/freedesktop/portal/desktop/session/{sender_path(sender)}/{token}"
        self.sessions.append(Session(self.bus, path))
        say("create-session", path)
        return self.answer(sender, options, 0, {"session_handle": dbus.String(path)})

    @dbus.service.method(SCREENCAST, in_signature="oa{sv}", out_signature="o", sender_keyword="sender")
    def SelectSources(self, session, options, sender=None):
        refuse = os.path.exists(os.path.join(self.control, "refuse"))
        say("select-sources", "types", int(options.get("types", 0)), "cursor", int(options.get("cursor_mode", 0)),
            "refused" if refuse else "ok")
        if refuse and self.session(session):
            self.session(session).open = False
        return self.answer(sender, options, 1 if refuse else 0, {})

    @dbus.service.method(SCREENCAST, in_signature="osa{sv}", out_signature="o", sender_keyword="sender")
    def Start(self, session, parent, options, sender=None):
        say("start", "node", self.node)
        if self.session(session):
            self.session(session).started = True
        stream = dbus.Struct(
            (
                dbus.UInt32(self.node),
                dbus.Dictionary(
                    {
                        "size": dbus.Struct((dbus.Int32(self.size[0]), dbus.Int32(self.size[1])), signature=None),
                        "source_type": dbus.UInt32(1),
                    },
                    signature="sv",
                ),
            ),
            signature=None,
        )
        streams = dbus.Array([stream], signature="(ua{sv})")
        return self.answer(sender, options, 0, {"streams": streams})

    @dbus.service.method(SCREENCAST, in_signature="oa{sv}", out_signature="h")
    def OpenPipeWireRemote(self, session, options):
        remote = os.environ.get("PIPEWIRE_REMOTE", "pipewire-0")
        path = remote if remote.startswith("/") else os.path.join(os.environ["XDG_RUNTIME_DIR"], remote)
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(path)
        say("open-pipewire-remote", path)
        fd = dbus.types.UnixFd(sock.fileno())
        sock.close()
        return fd

    @dbus.service.method(PROPS, in_signature="ss", out_signature="v")
    def Get(self, interface, name):
        return self.GetAll(interface)[name]

    @dbus.service.method(PROPS, in_signature="s", out_signature="a{sv}")
    def GetAll(self, interface):
        if interface != SCREENCAST:
            return dbus.Dictionary({}, signature="sv")
        return dbus.Dictionary(
            {
                "AvailableSourceTypes": dbus.UInt32(3),
                "AvailableCursorModes": dbus.UInt32(7),
                "version": dbus.UInt32(5),
            },
            signature="sv",
        )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bus", required=True)
    parser.add_argument("--node", type=int, required=True)
    parser.add_argument("--width", type=int, default=1280)
    parser.add_argument("--height", type=int, default=720)
    parser.add_argument("--control", required=True)
    args = parser.parse_args()
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.bus.BusConnection(args.bus)
    name = dbus.service.BusName("org.freedesktop.portal.Desktop", bus, do_not_queue=True)
    portal = Portal(bus, args.node, args.width, args.height, args.control)
    say("ready", name.get_name(), "node", args.node)
    loop = GLib.MainLoop()
    try:
        loop.run()
    except KeyboardInterrupt:
        pass
    del portal
    sys.exit(0)


if __name__ == "__main__":
    main()

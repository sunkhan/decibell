"""X11 fallback for window detection and focus (no KWin scripting: another X11 window manager, or XWayland clients
under a Wayland compositor that is not KWin). Uses python-xlib: PropertyNotify on the root window for
_NET_CLIENT_LIST (a window the WM manages, i.e. mapped) and _NET_ACTIVE_WINDOW; the owner pid from _NET_WM_PID, or
from the X-Resource extension (XResQueryClientIds, LocalClientPID) when the client does not set it.

Under a Wayland session this only sees X11 (XWayland) windows: native Wayland windows never reach the X root.
"""
import select
import threading
import time


class X11Watch:
    def __init__(self, on_window=None, on_active=None):
        from Xlib import X, display
        self.X = X
        self.d = display.Display()
        self.root = self.d.screen().root
        self.a_list = self.d.intern_atom('_NET_CLIENT_LIST')
        self.a_active = self.d.intern_atom('_NET_ACTIVE_WINDOW')
        self.a_pid = self.d.intern_atom('_NET_WM_PID')
        self.a_name = self.d.intern_atom('_NET_WM_NAME')
        self.a_utf8 = self.d.intern_atom('UTF8_STRING')
        self.on_window = on_window
        self.on_active = on_active
        self.has_res = self.d.has_extension('X-Resource')
        self.root.change_attributes(event_mask=X.PropertyChangeMask)
        self.known = set(self._clients())
        self.d.flush()
        self._stop = threading.Event()
        self.thread = threading.Thread(target=self._loop, name='x11-watch', daemon=True)
        self.thread.start()

    @staticmethod
    def available():
        try:
            from Xlib import display
            display.Display().close()
            return True
        except Exception:
            return False

    def _clients(self):
        p = self.root.get_full_property(self.a_list, self.X.AnyPropertyType)
        return list(p.value) if p else []

    def pid_of(self, xid):
        try:
            w = self.d.create_resource_object('window', xid)
            p = w.get_full_property(self.a_pid, self.X.AnyPropertyType)
            if p and len(p.value):
                return int(p.value[0])
        except Exception:
            pass
        if self.has_res:
            try:
                from Xlib.ext import res
                r = self.d.res_query_client_ids([{'client': xid, 'mask': res.LocalClientPIDMask}])
                for cid in r.ids:
                    if cid.spec.mask & res.LocalClientPIDMask and cid.value:
                        return int(cid.value[0])
            except Exception:
                pass
        return 0

    def info(self, xid):
        w = self.d.create_resource_object('window', xid)
        caption = ''
        try:
            p = w.get_full_property(self.a_name, self.a_utf8)
            caption = p.value.decode('utf-8', 'replace') if p else (w.get_wm_name() or '')
        except Exception:
            pass
        cls = ''
        try:
            c = w.get_wm_class()
            cls = c[1] if c else ''
        except Exception:
            pass
        return {'pid': self.pid_of(xid), 'caption': caption, 'cls': cls, 'id': 'x11:%#x' % xid, 'normal': True}

    def _loop(self):
        fd = self.d.fileno()
        while not self._stop.is_set():
            r, _, _ = select.select([fd], [], [], 0.5)
            if not r and not self.d.pending_events():
                continue
            while self.d.pending_events():
                ev = self.d.next_event()
                if ev.type != self.X.PropertyNotify:
                    continue
                now = time.time()
                if ev.atom == self.a_list and self.on_window:
                    cur = self._clients()
                    for xid in cur:
                        if xid not in self.known:
                            info = self.info(xid)
                            info.update({'k': 'added', 't': now * 1000.0, 'rx': now})
                            self.on_window(info)
                    self.known = set(cur)
                elif ev.atom == self.a_active and self.on_active:
                    p = self.root.get_full_property(self.a_active, self.X.AnyPropertyType)
                    xid = int(p.value[0]) if p and len(p.value) else 0
                    info = self.info(xid) if xid else {'pid': 0}
                    info.update({'k': 'activated', 't': now * 1000.0, 'rx': now})
                    self.on_active(info)

    def stop(self):
        self._stop.set()
        self.thread.join(2)
        try:
            self.d.close()
        except Exception:
            pass

"""Reads the X11 CLIPBOARD selection as another X client does (python-xlib), INCR transfers included.

    python3 -I x11_clipboard.py targets        # the offered targets, one per line
    python3 -I x11_clipboard.py image/png      # the bytes of that target, on stdout

Exit 3 when the owner refuses the target, 4 on a timeout. Uses $DISPLAY.
"""
import sys
import time

from Xlib import X, display

mode = sys.argv[1]
d = display.Display()
w = d.screen().root.create_window(0, 0, 1, 1, 0, X.CopyFromParent, event_mask=X.PropertyChangeMask)
clipboard = d.intern_atom('CLIPBOARD')
prop = d.intern_atom('DECIBELL_TEST_READ')
incr = d.intern_atom('INCR')
target = d.intern_atom('TARGETS' if mode == 'targets' else mode)
deadline = time.time() + 10


def next_event():
    while time.time() < deadline:
        if d.pending_events():
            return d.next_event()
        time.sleep(0.002)
    sys.exit(4)


w.convert_selection(clipboard, target, prop, X.CurrentTime)
d.flush()
while True:
    e = next_event()
    if e.type == X.SelectionNotify:
        break
if e.property == X.NONE:
    sys.exit(3)
reply = w.get_full_property(prop, X.AnyPropertyType, sizehint=1 << 22)
if reply is None:
    sys.exit(3)
if reply.property_type == incr:
    w.delete_property(prop)
    d.flush()
    data = bytearray()
    while True:
        e = next_event()
        if e.type != X.PropertyNotify or e.atom != prop or e.state != X.PropertyNewValue:
            continue
        piece = w.get_full_property(prop, X.AnyPropertyType, sizehint=1 << 22)
        w.delete_property(prop)
        d.flush()
        if piece is None or len(piece.value) == 0:
            break
        data.extend(bytes(piece.value))
    sys.stdout.buffer.write(bytes(data))
elif mode == 'targets':
    for atom in reply.value:
        print(d.get_atom_name(atom))
else:
    value = reply.value
    sys.stdout.buffer.write(value if isinstance(value, bytes) else bytes(value))

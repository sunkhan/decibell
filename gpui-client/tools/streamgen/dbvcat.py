#!/usr/bin/env python3
"""tools/streamgen/dbvcat.py <out.dbv> <seconds> <a.dbv> <b.dbv> ...: a DBV1 stream that switches between the inputs' pictures
(same codec, any size) every <seconds> of each input: a mid-stream resolution change for the decoder. Each
part starts on a keyframe (the inputs start with one and have one every 4 s; <seconds> must be a multiple of 4).
The header carries the first input's size and fps (the player paces every part at that fps)."""
import struct, sys
out, seconds, inputs = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
with open(out, "wb") as o:
    for i, path in enumerate(inputs):
        with open(path, "rb") as f:
            head = f.read(20)
            fps = struct.unpack("<I", head[16:20])[0]
            if i == 0:
                o.write(head); codec = head[4]
            assert head[:4] == b"DBV1" and head[4] == codec, "same codec, DBV1 only"
            for _ in range(fps * seconds):
                rec = f.read(8)
                if len(rec) < 8: break
                size = struct.unpack("<I", rec[:4])[0]
                o.write(rec); o.write(f.read(size))

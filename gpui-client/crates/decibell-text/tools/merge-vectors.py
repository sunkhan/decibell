#!/usr/bin/env python3
"""Packs a corpus and Chromium's results for it into tests/data/fidelity.json.gz, the vector
file `cargo test -p decibell-text --test fidelity` reads (no Electron needed there).

    tools/merge-vectors.py corpus.json chromium-1.json chromium-2.json > tests/data/fidelity.json.gz

Lengths are stored in 1/128 px (Chromium's LayoutUnit at scale 2), which is exact for
everything the page reports. Per case and scale: [height, [start, width, top or null]...].
The texts of the fixture cases are stored once per message, not once per width.
"""
import gzip
import json
import sys

corpus = json.load(open(sys.argv[1]))
results = [json.load(open(path)) for path in sys.argv[2:]]
q = lambda value: None if value is None else round(value * 128)

texts, text_index, cases = [], {}, []
for index, case in enumerate(corpus["cases"]):
    key = json.dumps(case["segs"], ensure_ascii=False)
    if key not in text_index:
        text_index[key] = len(texts)
        texts.append(case["segs"])
    packed = [text_index[key], case["style"], case["w"], case["wrap"], case["id"].split("/")[0]]
    for result in results:
        got = result["cases"][index]
        assert got["id"] == case["id"]
        packed.append([q(got["h"])] + [[line[0], q(line[1]), q(line[2])] for line in got["lines"]])
    cases.append(packed)

out = {
    "about": "decibell-text fidelity vectors; see tools/README.md",
    "user_agent": results[0]["userAgent"],
    "scales": [result["dsf"] for result in results],
    "styles": corpus["styles"],
    "texts": texts,
    "cases": cases,
}
data = json.dumps(out, ensure_ascii=False, separators=(",", ":")).encode()
sys.stdout.buffer.write(gzip.compress(data, 9, mtime=0))

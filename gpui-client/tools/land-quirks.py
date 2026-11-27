# Resolve a conflict in docs/electron-quirks.md while a landing pick is stopped (run from the landing tree's root): keep HEAD's rows, append the incoming ones renumbered after the highest row. Then fix the incoming commits' references to their old numbers by hand.
import re, sys
p = "gpui-client/docs/electron-quirks.md"
L = open(p, encoding="utf-8").read().split("\n")
while any(l.startswith("<<<<<<< ") for l in L):
    a = next(i for i, l in enumerate(L) if l.startswith("<<<<<<< "))
    m = next(i for i, l in enumerate(L) if l == "=======" and i > a)
    b = next(i for i, l in enumerate(L) if l.startswith(">>>>>>> ") and i > m)
    ours, theirs = L[a + 1:m], L[m + 1:b]
    top = max(int(x.group(1)) for l in L if not l.startswith(("<", ">", "=")) for x in [re.match(r"\| (\d+) \|", l)] if x and (L.index(l) < m or L.index(l) > b))
    out = []
    for l in theirs:
        x = re.match(r"\| (\d+) \|", l)
        if x:
            top += 1
            print(f"  theirs' row {x.group(1)} -> {top}")
            l = f"| {top} |" + l[x.end():]
        out.append(l)
    L[a:b + 1] = ours + out
open(p, "w", encoding="utf-8").write("\n".join(L))

# Resolve conflicts in gpui-client/PARITY.md, where every row is one line, row by row and three ways.
# Run from the landing tree while the cherry-pick is stopped:  parity-resolve.py gpui-client/PARITY.md
# It re-creates the conflict with the merge base shown (diff3), then for each row:
#   the incoming side left it as the base had it  -> HEAD's row (someone else's later work stays);
#   HEAD left it as the base had it               -> the incoming row;
#   both changed it                               -> the incoming cells with HEAD's Windows cell.
import re, subprocess, sys
p = sys.argv[1]
subprocess.run(["git", "checkout", "--conflict=diff3", "--", p], check=True)
L = open(p, encoding="utf-8").read().split("\n")
rid = lambda l: (re.match(r"\| (\d+\.\d+[a-z]?) \|", l) or [None, None])[1]
cells = lambda l: l.split("|")[1:-1]
stats = {"head": 0, "incoming": 0, "both": 0}
while any(l.startswith("<<<<<<< ") for l in L):
    a = next(i for i, l in enumerate(L) if l.startswith("<<<<<<< "))
    o = next(i for i, l in enumerate(L) if l.startswith("||||||| ") and i > a)
    m = next(i for i, l in enumerate(L) if l == "=======" and i > o)
    b = next(i for i, l in enumerate(L) if l.startswith(">>>>>>> ") and i > m)
    ours, base, theirs = L[a + 1:o], L[o + 1:m], L[m + 1:b]
    ours_by = {rid(l): l for l in ours if rid(l)}
    base_by = {rid(l): l for l in base if rid(l)}
    out = []
    if not any(rid(l) for l in theirs + ours):
        out = ours + [l for l in theirs if l not in ours]          # not table rows: keep both
    else:
        seen = set()
        for l in theirs:
            r = rid(l)
            if not r:
                out.append(l); continue
            seen.add(r)
            mine, was = ours_by.get(r), base_by.get(r)
            if mine is None:
                out.append(l)
            elif l == was:
                out.append(mine); stats["head"] += 1
            elif mine == was:
                out.append(l); stats["incoming"] += 1
            else:
                oc, tc = cells(mine), cells(l)
                if len(oc) == len(tc) == 6:
                    tc[4] = oc[4]; l = "|" + "|".join(tc) + "|"
                out.append(l); stats["both"] += 1
        out += [l for l in ours if rid(l) and rid(l) not in seen]
    L[a:b + 1] = out
open(p, "w", encoding="utf-8").write("\n".join(L))
print(f"PARITY: {stats['head']} rows kept as HEAD has them, {stats['incoming']} taken from the incoming side, {stats['both']} changed by both (incoming cells, HEAD's Windows cell)")

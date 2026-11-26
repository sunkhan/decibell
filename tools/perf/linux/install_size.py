"""install-size: apparent size and size on disk (du), file count and the largest files of installed clients.

Measures only: nothing under the paths is executed. Apparent size is `du -sb` (bytes of content, hard links counted
once); on disk is `du -s --block-size=1` (allocated blocks: what the install costs the filesystem; on btrfs with
compression it can be well under the apparent size). Symlinks are not followed.
"""
import argparse
import json
import os
import subprocess
import sys

import common


def du(path, apparent):
    cmd = ['du', '-s', '-b' if apparent else '--block-size=1', path]
    out = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if out.returncode != 0 and not out.stdout:
        raise RuntimeError(out.stderr.strip())
    return int(out.stdout.split()[0])


def walk(path, top):
    files = 0
    dirs = 0
    links = 0
    largest = []
    seen = set()
    if os.path.isfile(path) and not os.path.islink(path):
        st = os.lstat(path)
        return 1, 0, 0, [(st.st_size, path)]
    for root, dnames, fnames in os.walk(path):
        dirs += len(dnames)
        for f in fnames:
            p = os.path.join(root, f)
            try:
                st = os.lstat(p)
            except OSError:
                continue
            if os.path.islink(p):
                links += 1
                continue
            if (st.st_dev, st.st_ino) in seen:
                continue
            seen.add((st.st_dev, st.st_ino))
            files += 1
            largest.append((st.st_size, p))
    largest.sort(reverse=True)
    return files, dirs, links, largest[:top]


def mb(b):
    return round(b / 1048576.0, 1)  # PowerShell's 1MB (2^20), as measure.ps1 / startup.ps1 and the baseline use


def main(argv):
    ap = argparse.ArgumentParser(prog='install-size.sh', description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter,
                                 epilog='example:\n  install-size.sh /opt/Decibell ~/src/decibell/gpui-client/target/release/'
                                        'decibell')
    ap.add_argument('paths', nargs='+', help='install directories or files')
    ap.add_argument('--top', type=int, default=15, help='largest files to list per path (default 15)')
    ap.add_argument('--out', help='output directory (default tools/perf/results/install-size-<time>)')
    ap.add_argument('--label', default='')
    a = ap.parse_args(argv)

    results = []
    for path in a.paths:
        path = os.path.abspath(path)
        if not os.path.exists(path):
            print('install-size: %s does not exist' % path, file=sys.stderr)
            return 2
        apparent = du(path, True)
        disk = du(path, False)
        files, dirs, links, largest = walk(path, a.top)
        results.append({'path': path, 'apparent_bytes': apparent, 'disk_bytes': disk, 'apparent_mb': mb(apparent),
                        'disk_mb': mb(disk), 'files': files, 'dirs': dirs, 'symlinks': links,
                        'largest': [{'bytes': s, 'mb': mb(s), 'path': os.path.relpath(p, path) if p != path else
                                     os.path.basename(p)} for s, p in largest]})

    out = common.out_dir(a.out, 'install-size' + ('-' + a.label if a.label else ''))
    common.write_json(os.path.join(out, 'install-size.json'),
                      {'meta': {'tool': 'tools/perf/install-size.sh', 'label': a.label, 'when': common.iso(),
                                'units': 'MB = 2^20 bytes (PowerShell 1MB, like the Windows scripts)'},
                       'paths': results})
    for r in results:
        print('%s' % r['path'])
        print('  apparent %.1f MB, on disk %.1f MB, %d files, %d dirs, %d symlinks' % (
            r['apparent_mb'], r['disk_mb'], r['files'], r['dirs'], r['symlinks']))
        for f in r['largest']:
            print('  %10.1f MB  %s' % (f['mb'], f['path']))
    print('wrote %s/install-size.json' % out)
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

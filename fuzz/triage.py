"""Buckets libFuzzer crash files by signature and keeps the smallest input per bucket.

usage: python -I triage.py <fuzz_ioctl.exe> <crash files or dirs...>

Signature = FUZZ_CHECK message, or ASan bug type plus the first stack frame
in driver/harness code.
"""
import collections
import pathlib
import re
import subprocess
import sys

FRAME = re.compile(r"#\d+ 0x[0-9a-f]+ in (\S+) .*\\((?:crosecbus|fuzz)\\[^\\]+\.c):(\d+)")
ASAN = re.compile(r"ERROR: AddressSanitizer: (\S+)")
CHECK = re.compile(r"FUZZ_CHECK failed: (.*) \(.*\\([^\\]+:\d+)\)")
SKIP_FRAMES = {"fi_bytes", "LLVMFuzzerTestOneInput"}


def signature(output):
    m = CHECK.search(output)
    if m:
        return f"FUZZ_CHECK {m.group(1)} @ {m.group(2)}"
    m = ASAN.search(output)
    if m:
        for func, file, line in FRAME.findall(output):
            if func not in SKIP_FRAMES:
                return f"ASan {m.group(1)} in {func} @ {file}:{line}"
        return f"ASan {m.group(1)}"
    return "no crash reproduced"


def main():
    exe = str(pathlib.Path(sys.argv[1]).resolve())
    files = []
    for arg in sys.argv[2:]:
        p = pathlib.Path(arg)
        files.extend(sorted(p.glob("crash-*")) if p.is_dir() else [p])

    buckets = collections.defaultdict(list)
    for f in files:
        run = subprocess.run([exe, str(f)], capture_output=True, text=True, errors="replace", timeout=60)
        buckets[signature(run.stdout + run.stderr)].append(f)

    for sig, members in sorted(buckets.items(), key=lambda kv: -len(kv[1])):
        smallest = min(members, key=lambda f: f.stat().st_size)
        print(f"{len(members):6}  {sig}\n        smallest: {smallest} ({smallest.stat().st_size} bytes)")


if __name__ == "__main__":
    main()

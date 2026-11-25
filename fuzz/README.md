# Fuzzing crosecbus

A libFuzzer harness that runs the driver's real userspace IOCTL handlers
(`userspaceQueue.c`) and EC transports (`comm-lpc.c`, `comm-mec_lpc.c`) in
user mode, under AddressSanitizer, against a simulated EC whose replies are
fuzzer-controlled.

- `shim/fuzz_shim.h` is force-included in place of the WDK: NT types, port I/O
  and the few WDF/Ke/Ex calls used. The driver's own types come from
  `crosecbus/crosec_types.h`, shared with the real build.
- `fake_wdf.c` models a `METHOD_BUFFERED` request (one exactly-sized system
  buffer, so ASan catches overruns) and checks lock/completion discipline.
- `fake_ec.c` simulates LPC v2, LPC v3 and Microchip MEC (EMI) ECs. It
  validates every request the driver sends (checksums, sizes, blocked flash
  commands never reaching the EC) and answers with fuzz-chosen results,
  lengths, payloads and protocol violations.
- `fuzz_ioctl.c` is the fuzz target; the input layout is documented at the top.

## Requirements

Visual Studio 2022 or later with the C++ x64 tools (MSVC's `/fsanitize=fuzzer`).
No WDK needed.

## Usage

```powershell
fuzz\build.ps1                                   # -> fuzz\build\fuzz_ioctl.exe

# Fuzz (Ctrl+C to stop); new coverage is kept in fuzz\build\corpus
fuzz\build\fuzz_ioctl.exe fuzz\build\corpus fuzz\corpus -dict=fuzz\ec.dict -artifact_prefix=fuzz\build\

# Several workers, keep going after crashes
fuzz\build\fuzz_ioctl.exe fuzz\build\corpus fuzz\corpus -dict=fuzz\ec.dict -fork=4 -ignore_crashes=1 -artifact_prefix=fuzz\build\

# Replay regressions (what CI does first)
fuzz\build\fuzz_ioctl.exe -runs=0 fuzz\regressions fuzz\corpus
```

## Triage

```powershell
python -I fuzz\triage.py fuzz\build\fuzz_ioctl.exe fuzz\build   # bucket crashes by signature
fuzz\build.ps1 -Trace                                          # build fuzz_ioctl_trace.exe
fuzz\build\fuzz_ioctl_trace.exe fuzz\build\crash-<hash>        # log each IOCTL and its result
```

When a crash is fixed, copy its (minimized: `-minimize_crash=1 -runs=10000`)
input into `regressions/` with a descriptive name.

`corpus/` holds hand-written seeds; regenerate with `python -I fuzz\make_seeds.py`.

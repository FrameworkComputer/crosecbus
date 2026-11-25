# crosecbus-rs

Rust port of [coolstar/crosecbus](https://github.com/coolstar/crosecbus), the
Chrome EC bus driver for Windows, built on
[windows-drivers-rs](https://github.com/microsoft/windows-drivers-rs) in the
style of the
[Windows Rust driver samples](https://github.com/microsoft/Windows-rust-driver-samples).

## Layout

| Path | What |
| --- | --- |
| `crosec-proto/` | `no_std` EC host command transport: LPC v2, LPC v3, MEC LPC (EMI window), memmap reads, probing. No WDK dependency; hardware access goes through a `Hal` trait, so it is unit tested on the host against a simulated EC. |
| `crosecbus/` | KMDF driver: PnP/power callbacks, passive-level ISR (MKBP button events to `\Callback\CsKeyboardSettingsCallbackAPI`), the v1/v2 kernel query interfaces, and the user mode IOCTLs. |
| `crosecbus/crosecbus.inx` | INF template (same hardware ID and settings as the C driver). |

Mapping from the C sources:

| C | Rust |
| --- | --- |
| `comm-lpc.c`, `comm-mec_lpc.c`, `comm-host.h` | `crosec-proto/src/lib.rs` |
| `ec_commands.h` (parts used by the driver) | `crosec-proto/src/commands.rs` |
| `crosecbus.c`, `driver.h` | `crosecbus/src/{driver,device,lib}.rs` |
| `userspaceQueue.c` | `crosecbus/src/queue.rs` |
| `READ_PORT_UCHAR` etc. | `crosecbus/src/hal.rs` (`in`/`out` instructions) |

## Compatibility

Binary interfaces are unchanged:

* `\Device\CrosEC` / `\DosDevices\GOOG0004`, `IOCTL_CROSEC_XCMD` (`0x80EC_E004`)
  and `IOCTL_CROSEC_RDMEM` (`0x80EC_6008`) with the same `CROSEC_COMMAND` /
  `CROSEC_READMEM` layouts, size checks, deny list and status codes, so
  `framework_tool` and other existing clients work as before.
* `GUID_CROSEC_INTERFACE_STANDARD` (v1) and `GUID_CROSEC_INTERFACE_STANDARD_V2`
  with the same structure layouts, for child drivers (croskblight,
  crossensors, croseci2c, ...).

Intentional differences from the C driver:

* **Deny list bypass fixed:** the user mode flash/PD-update deny list now
  checks the 16-bit command the EC actually receives. The C driver compared
  the full 32-bit value and then truncated it, so `0x10012` reached the EC as
  `EC_CMD_FLASH_WRITE`.
* **MEC buffer overrun fixed:** an unaligned MEC transfer shorter than the rest
  of its dword no longer writes past the end of the buffer. This happened on
  every unaligned memmap string read in the C driver.
* `IOCTL_CROSEC_RDMEM`'s `offset + bytes` check can no longer overflow, and
  `CheckFeatures` / `ReadEcMem` reject out-of-range arguments instead of
  indexing out of bounds.
* `IOCTL_CROSEC_XCMD` requires at least a full 20-byte header (the C driver
  asked WDF for `sizeof(pointer)` bytes and then read 20).
* Protocol v2 rejects requests over `EC_PROTO2_MAX_PARAM_SIZE` instead of
  truncating the length byte.
* The keyboard callback object is released in `EvtDeviceSelfManagedIoCleanup`
  instead of `EvtDeviceReleaseHardware`, so it survives a resource rebalance.
* Transport state lives in the device context rather than globals, and the
  wait locks are parented to the device instead of the driver.
* Tracing goes through `DbgPrint` (`wdk::println!`) instead of WPP.
* The S0ix notification code (commented out in the C driver) was not ported,
  nor the unused `EvtIoStop`/`EvtDeviceD0Exit` stubs.

## Building

Requirements are the same as for the Rust driver samples: the WDK (or EWDK),
LLVM/Clang, and `cargo-make`.

```shell
cargo build --release                 # builds target/release/crosecbus.dll (the .sys)
cargo test -p crosec-proto            # protocol tests against the simulated EC
cargo clippy --all-targets -- -D warnings
cargo make                            # stamp INF, build catalog, test-sign into target/*/crosecbus_package
```

CI (`.github/workflows/ci.yml`) runs the formatting check, the protocol tests,
clippy and the driver build on stable and nightly, and uploads the release
package from `cargo make` as the `crosecbus_package` artifact (test-signed
only).

`cargo make` symlinks its makefiles into `target/`, which on Windows needs
Developer Mode (or an elevated shell). Its signing step also creates a
self-signed test certificate in the current user's `WDRTestCertStore`.

Without Developer Mode, `scripts/package-local.ps1` does the same build,
stamp, `infverif`, `inf2cat` and test-signing steps. It signs with a
self-signed certificate in `CurrentUser\My`, created on first use, and writes
the package to `target\release\crosecbus_package`.

## Installing a test build

With test signing on (`bcdedit /set testsigning on`, Secure Boot off), run
`install-testsigned.ps1` from the package folder as Administrator. It trusts
the package's signing certificate, stages the driver, and force-installs it
on `ACPI\FRMWC004`. Otherwise PnP keeps Framework's WHQL-signed driver. The
previous driver stays in the driver store, so Device Manager -> Roll Back
Driver switches back. The script is included in CI artifacts as well.

## Testing on hardware

Run these as Administrator.

* `scripts/smoke-test.ps1` checks device state and runs the `RDMEM` and
  `EC_CMD_HELLO` IOCTL probes and `framework_tool --versions/--power`. Each
  check has a timeout, so a hung driver shows up as a failure. Pass
  `-Baseline <folder>` to diff against an earlier run, e.g. one taken with
  Framework's C driver.
* `scripts/test-power.ps1 [-Mode standby|hibernate|both] [-Cycles 3]` uses
  `pwrtest` to cycle through Modern Standby (the device stays in D0) and/or
  Hibernate (D3, so `EvtDeviceD0Entry` runs again). It runs the smoke test
  before and after each mode and collects the pwrtest logs, System events
  (summary and `.evtx`) and a sleep study into `target/power-test/<timestamp>/`.
  The machine really sleeps, so save your work first.

## License

Apache-2.0, as with the original driver. See `LICENSE.txt`.

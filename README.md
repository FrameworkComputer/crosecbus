Chrome EC Bus Driver

* NOTE: This driver does NOT expose services to userspace directly
* To use this driver, either ACPI must expose child devices, or you must modify this driver to enumerate child devices for other drivers to attach to

Known ACPI IDs:
* ACPI\GOOG0004: Chrome EC Bus (This driver)
* ACPI\GOOG0002: Chrome EC Keyboard Backlight (Technically could be controlled via ACPI, but crosecbus is more reliable) - https://github.com/coolstar/croskblight
* ACPI\GOOG0003: Chrome EC PD Notify (used for USB-C)
* ACPI\GOOG0006: Chrome EC Sensor Hub - https://github.com/coolstar/crossensors
* ACPI\GOOG0007: Chrome EC Vivaldi Keyboard Settings - https://github.com/coolstar/crosecvivaldi
* ACPI\GOOG000A: Chrome EC Keyboard - https://github.com/coolstar/croskeyboard4
* ACPI\GOOG000B: Pixel Slate Base (also used for Sensor Hub) - https://github.com/coolstar/crossensors
* ACPI\GOOG0012: Chrome EC I2C Passthrough - https://github.com/coolstar/croseci2c
* ACPI\GOOG0013: Chrome EC Audio Codec (Usually DMIC over I2S) - https://github.com/coolstar/croseccodec
* ACPI\GOOG0014: Chrome EC USB-C
* ACPI\GOOG0015: Chrome EC Trackpoint
* ACPI\GOOG0016: Chrome OS GPIOs

IDs not covered by crosec:
* ACPI\GOOG000C: Wilco EC (not this driver)
* ACPI\GOOG000D: Wilco EC Event (unused in Windows)
* ACPI\GOOG000E: Wilco EC UCSI (covered by in-box UCSI driver)

Protocols Implemented:
* LPC v2
* LPC v3 (Most Chromebooks)
* MEC LPC (Braswell/Skylake Chromebooks, Framework laptop)

Note: Framework laptop does not implement GOOG0004 ACPI device. Override DSDT/SSDT with testsigning or with OpenCore to add it. (See https://github.com/coreboot/coreboot/blob/master/src/ec/google/chromeec/acpi/cros_ec.asl for an example)

Tested on HP Chromebook 14b (Ryzen 3 3250C)

## EC console log

The EC keeps its console output in a small ring buffer, so older output gets overwritten.
The driver copies it into the Windows Event Log so it survives reboots.

* Channel: `Framework-CrosEcBus/Console` (Event Viewer: Applications and Services Logs > Framework-CrosEcBus > Console)
* One event per console line (event ID 1). Every time the driver loads, typically once per boot, it writes a
  marker with the EC firmware version (event ID 2), then the whole current EC buffer, which includes output
  from before Windows started and from the end of the previous boot.
* The log is capped at 10 MB; the oldest events are overwritten. Change the size with
  `wevtutil sl Framework-CrosEcBus/Console /ms:<bytes>`
* Requires Windows 10 1809 or later

Read it as text (PowerShell):

```powershell
# Whole log, oldest first
Get-WinEvent -LogName Framework-CrosEcBus/Console -Oldest |
  ForEach-Object { "{0:yyyy-MM-dd HH:mm:ss} {1}" -f $_.TimeCreated, $_.Message }

# Current boot only
$start = Get-WinEvent -LogName Framework-CrosEcBus/Console -FilterXPath "*[System[EventID=2]]" -MaxEvents 1
Get-WinEvent -LogName Framework-CrosEcBus/Console -Oldest |
  Where-Object TimeCreated -ge $start.TimeCreated | ForEach-Object Message

# Save to a file
Get-WinEvent -LogName Framework-CrosEcBus/Console -Oldest | ForEach-Object Message | Set-Content ec-console.txt
```

Or from cmd: `wevtutil qe Framework-CrosEcBus/Console /f:text`

Settings (DWORD values under the device's hardware key, `HKLM\SYSTEM\CurrentControlSet\Enum\<device instance>\Device Parameters\Settings`,
take effect after the device is restarted):

* `ConsoleLogEnabled`: 1 (default) to log, 0 to disable
* `ConsoleLogPollMs`: how often to read the EC console, in milliseconds (default 15000, minimum 1000)

Note: the driver and tools such as `framework_tool --console` or `ectool console` share the same EC read
position. If you use them while logging is enabled, each side will miss some of the output.
Set `ConsoleLogEnabled` to 0 if you need those tools to see everything.

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

### Troubleshooting the EC console log

The version number alone doesn't tell builds apart. To check that the installed driver is the one you built,
compare hashes:

```powershell
$dev = Get-PnpDevice -InstanceId 'ACPI\FRMWC004*'
Get-PnpDeviceProperty -InstanceId $dev.InstanceId -KeyName DEVPKEY_Device_DriverInfPath, DEVPKEY_Device_DriverVersion, DEVPKEY_Device_DriverDate
(Get-CimInstance Win32_SystemDriver -Filter "Name='CrosEcBus'").PathName   # the .sys that is running
Get-FileHash <that path>, <your build>\crosecbus.sys                         # should match
```

Check each link in the chain:

```powershell
# 1. Provider registered (only exists if the INF's Events section was installed)
wevtutil gp Framework-CrosEcBus

# 2. Channel exists, is enabled ("enabled: true") and has "isolation: System"
wevtutil gl Framework-CrosEcBus/Console
#    If it says false: wevtutil sl Framework-CrosEcBus/Console /e:true
#    If it says "isolation: Application" (installed by an older build of this branch), events from
#    the driver never reach the log. Fix it without reinstalling:
#      Set-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WINEVT\Channels\Framework-CrosEcBus/Console' Isolation 1
#      wevtutil sl Framework-CrosEcBus/Console /e:false; wevtutil sl Framework-CrosEcBus/Console /e:true
#    (The registry uses 1 for System; the INF's Isolation directive uses 2.)

# 3. Number of events in the log ("numberOfLogRecords")
wevtutil gli Framework-CrosEcBus/Console

# 4. The Event Log service is listening to the driver. The driver only reads the
#    EC console while this is true, so the data isn't consumed when nobody records it.
#    Expect "Framework-CrosEcBus" with KeywordsAny 0x8000000000000000.
logman query EventLog-System -ets | Select-String -Context 0,4 CrosEcBus

# 5. The driver has the provider registered (a row with PID 0x00000000 = kernel)
logman query providers Framework-CrosEcBus

# 6. Settings: ConsoleLogEnabled should be 1
Get-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Enum\$((Get-PnpDevice -InstanceId 'ACPI\FRMWC004*').InstanceId)\Device Parameters\Settings"
```

The driver writes event ID 2 on the first poll after it starts, even if the EC buffer is empty. If the log
stays empty for more than one poll interval, capture the driver's WPP trace (as admin) while restarting the device:

```powershell
logman create trace CrosEcBusWpp -p '{73e3b785-f5fb-423e-94a9-56627fea9053}' 0xFFFFFFFF 0xFF -o crosecbus.etl -ets
pnputil /restart-device "ACPI\FRMWC004\1"
# wait at least one poll interval
logman stop CrosEcBusWpp -ets
```

Decoding needs the TMF format strings from the matching `crosecbus.pdb` (CI uploads it next to the `.sys`).
With the WDK: `tracepdb -f crosecbus.pdb -p tmf`, then `tracefmt crosecbus.etl -p tmf -o crosecbus.txt`.
Without the WDK, pull them out of the PDB and decode with the built-in `tracerpt`:

```powershell
New-Item -ItemType Directory -Force tmf | Out-Null
$s = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes("$PWD\crosecbus.pdb"))
foreach ($r in [regex]::Matches($s, 'TMF:\x00((?:[^\x00]+\x00)+?)\x00')) {
  $lines = $r.Groups[1].Value.TrimEnd([char]0).Split([char]0)
  Add-Content -Path "tmf\$($lines[0].Split(' ')[0]).tmf" -Value ($lines -join "`r`n") -Encoding ASCII
}
tracerpt crosecbus.etl -o crosecbus.csv -of CSV -tp tmf -y
```

To tell whether the driver writes events at all, independent of the Event Log service, record the provider
in a session of your own. Starting it also makes the driver poll right away:

```powershell
logman create trace CrosEcBusEvents -p Framework-CrosEcBus 0xFFFFFFFFFFFFFFFF 0xFF -o events.etl -ets
# wait a few seconds
logman stop CrosEcBusEvents -ets
Get-WinEvent -Path events.etl -Oldest | Select-Object TimeCreated, Id, Message
```

If events show up here but not in the channel, the problem is the channel configuration (see step 2).

Messages from the console logger to look for: `EC console logging started, polling every N ms`,
`EC console logging disabled`, `Failed to start EC console logging`, `Console snapshot failed` and
`Console read failed`.

#include "driver.h"
#include "comm-host.h"

//
// Called by the mc.exe generated ETW enable callback, after the provider's
// enable bits have been updated. Must be declared before the generated header.
//
static VOID CrosEcConsoleLogEnableCallback(
	_In_ LPCGUID SourceId,
	_In_ ULONG ControlCode,
	_In_ UCHAR Level,
	_In_ ULONGLONG MatchAnyKeyword,
	_In_ ULONGLONG MatchAllKeyword,
	_In_opt_ PEVENT_FILTER_DESCRIPTOR FilterData,
	_Inout_opt_ PVOID CallbackContext);
#define MCGEN_PRIVATE_ENABLE_CALLBACK_V2 CrosEcConsoleLogEnableCallback

#include "crosecbusEvents.h"
#include "consolelog.tmh"

#define CROSEC_CONSOLE_POLL_MS_DEFAULT 15000
#define CROSEC_CONSOLE_POLL_MS_MIN     1000
// Upper bound on read commands per poll, in case the EC keeps producing output
#define CROSEC_CONSOLE_MAX_CHUNKS      256

// Device to kick when a consumer (the event log service) enables the provider
static KSPIN_LOCK g_ConsoleLogDeviceLock;
static PCROSECBUS_CONTEXT g_ConsoleLogDevice;

static EXT_CALLBACK CrosEcConsoleLogTimerCallback;
static KSTART_ROUTINE CrosEcConsoleLogThread;

VOID CrosEcConsoleLogRegisterProvider(VOID)
{
	KeInitializeSpinLock(&g_ConsoleLogDeviceLock);
	EventRegisterFramework_CrosEcBus();
}

VOID CrosEcConsoleLogUnregisterProvider(VOID)
{
	EventUnregisterFramework_CrosEcBus();
}

static VOID CrosEcConsoleLogSetDevice(_In_opt_ PCROSECBUS_CONTEXT pDevice)
{
	KIRQL oldIrql;
	KeAcquireSpinLock(&g_ConsoleLogDeviceLock, &oldIrql);
	g_ConsoleLogDevice = pDevice;
	KeReleaseSpinLock(&g_ConsoleLogDeviceLock, oldIrql);
}

static VOID CrosEcConsoleLogEnableCallback(
	_In_ LPCGUID SourceId,
	_In_ ULONG ControlCode,
	_In_ UCHAR Level,
	_In_ ULONGLONG MatchAnyKeyword,
	_In_ ULONGLONG MatchAllKeyword,
	_In_opt_ PEVENT_FILTER_DESCRIPTOR FilterData,
	_Inout_opt_ PVOID CallbackContext)
{
	UNREFERENCED_PARAMETER(SourceId);
	UNREFERENCED_PARAMETER(Level);
	UNREFERENCED_PARAMETER(MatchAnyKeyword);
	UNREFERENCED_PARAMETER(MatchAllKeyword);
	UNREFERENCED_PARAMETER(FilterData);
	UNREFERENCED_PARAMETER(CallbackContext);

	if (ControlCode != EVENT_CONTROL_CODE_ENABLE_PROVIDER)
		return;

	// Poll right away instead of waiting for the next timer tick
	KIRQL oldIrql;
	KeAcquireSpinLock(&g_ConsoleLogDeviceLock, &oldIrql);
	if (g_ConsoleLogDevice) {
		KeSetEvent(&g_ConsoleLogDevice->ConsoleLog.PollEvent, IO_NO_INCREMENT, FALSE);
	}
	KeReleaseSpinLock(&g_ConsoleLogDeviceLock, oldIrql);
}

static VOID CrosEcConsoleLogTimerCallback(
	_In_ PEX_TIMER Timer,
	_In_opt_ PVOID Context)
{
	UNREFERENCED_PARAMETER(Timer);

	PCROSECBUS_CONTEXT pDevice = (PCROSECBUS_CONTEXT)Context;
	if (pDevice) {
		KeSetEvent(&pDevice->ConsoleLog.PollEvent, IO_NO_INCREMENT, FALSE);
	}
}

//
// Like CrosEcCmdXferStatus, but returns the raw result (response size or
// negative error). Doesn't bump KernelAccessesWaiting: console logging is
// low priority and shouldn't make userspace IOCTLs return STATUS_RETRY.
//
_IRQL_requires_(PASSIVE_LEVEL)
static int CrosEcConsoleLogCommand(
	_In_ PCROSECBUS_CONTEXT pDevice,
	UINT16 command,
	UINT8 version,
	_In_reads_bytes_opt_(outsize) const void* outdata,
	int outsize,
	_Out_writes_bytes_opt_(insize) void* indata,
	int insize)
{
	if (!ec_command_proto) {
		return -EC_RES_UNAVAILABLE;
	}

	WdfWaitLockAcquire(pDevice->EcLock, NULL);
	int rv = ec_command_proto(command, version, outdata, outsize, indata, insize);
	WdfWaitLockRelease(pDevice->EcLock);

	return rv;
}

static VOID CrosEcConsoleLogEmitLine(_Inout_ PCROSEC_CONSOLE_LOG log)
{
	log->Line[log->LineLen] = '\0';
	EventWriteConsoleLine(NULL, log->Line);
	log->LineLen = 0;
}

// Split console output into lines. A trailing partial line is kept for later.
static VOID CrosEcConsoleLogAppend(
	_Inout_ PCROSEC_CONSOLE_LOG log,
	_In_reads_(len) const CHAR* data,
	int len)
{
	for (int i = 0; i < len; i++) {
		CHAR c = data[i];
		if (c == '\0')
			break;
		if (c == '\r')
			continue;
		if (c == '\n') {
			CrosEcConsoleLogEmitLine(log);
			continue;
		}

		if (log->LineLen >= CROSEC_CONSOLE_LINE_MAX - 1)
			CrosEcConsoleLogEmitLine(log);
		log->Line[log->LineLen++] = c;
	}
}

_IRQL_requires_(PASSIVE_LEVEL)
static VOID CrosEcConsoleLogWriteStarted(_In_ PCROSECBUS_CONTEXT pDevice)
{
	struct ec_response_get_version r = { 0 };

	int rv = CrosEcConsoleLogCommand(pDevice, EC_CMD_GET_VERSION, 0, NULL, 0, &r, sizeof(r));
	if (rv < 0) {
		RtlZeroMemory(&r, sizeof(r));
	}
	r.version_string_ro[sizeof(r.version_string_ro) - 1] = '\0';
	r.version_string_rw[sizeof(r.version_string_rw) - 1] = '\0';

	EventWriteConsoleLogStarted(NULL, r.version_string_ro, r.version_string_rw);
}

_IRQL_requires_(PASSIVE_LEVEL)
static VOID CrosEcConsoleLogPoll(_In_ PCROSECBUS_CONTEXT pDevice, BOOLEAN Final)
{
	PCROSEC_CONSOLE_LOG log = &pDevice->ConsoleLog;

	// Leave the data in the EC until someone is listening
	if (!EventEnabledConsoleLine())
		return;

	int rv = CrosEcConsoleLogCommand(pDevice, EC_CMD_CONSOLE_SNAPSHOT, 0, NULL, 0, NULL, 0);
	if (rv < 0) {
		TraceEvents(TRACE_LEVEL_ERROR, TRACE_CONSOLELOG, "Console snapshot failed: %d\n", rv);
		return;
	}

	struct ec_params_console_read_v1 params = { 0 };
	if (!log->DumpedInitial) {
		// First poll after load: dump the whole EC buffer, including
		// output from before the OS (or before the last reboot)
		CrosEcConsoleLogWriteStarted(pDevice);
		params.subcmd = CONSOLE_READ_NEXT;
		log->DumpedInitial = TRUE;
	}
	else {
		params.subcmd = CONSOLE_READ_RECENT;
	}

	BOOLEAN gotData = FALSE;
	for (int i = 0; i < CROSEC_CONSOLE_MAX_CHUNKS; i++) {
		RtlZeroMemory(log->ReadBuf, log->ReadBufSize);
		rv = CrosEcConsoleLogCommand(pDevice, EC_CMD_CONSOLE_READ, 1, &params, sizeof(params),
			log->ReadBuf, (int)log->ReadBufSize);
		if (rv < 0) {
			TraceEvents(TRACE_LEVEL_ERROR, TRACE_CONSOLELOG, "Console read failed: %d\n", rv);
			break;
		}
		// Empty string means no more output
		if (rv == 0 || log->ReadBuf[0] == '\0')
			break;

		gotData = TRUE;
		CrosEcConsoleLogAppend(log, log->ReadBuf, min(rv, (int)log->ReadBufSize));
	}

	// Don't hold back a partial line (e.g. a prompt) for more than one poll
	if (log->LineLen > 0 && (Final || !gotData))
		CrosEcConsoleLogEmitLine(log);
}

static VOID CrosEcConsoleLogThread(_In_ PVOID Context)
{
	PCROSECBUS_CONTEXT pDevice = (PCROSECBUS_CONTEXT)Context;
	PCROSEC_CONSOLE_LOG log = &pDevice->ConsoleLog;
	PVOID waitObjects[2] = { &log->StopEvent, &log->PollEvent };

	for (;;) {
		NTSTATUS status = KeWaitForMultipleObjects(ARRAYSIZE(waitObjects), waitObjects, WaitAny,
			Executive, KernelMode, FALSE, NULL, NULL);
		BOOLEAN stop = (status == STATUS_WAIT_0);

		// Read one last time when stopping
		CrosEcConsoleLogPoll(pDevice, stop);

		if (stop)
			break;
	}

	PsTerminateSystemThread(STATUS_SUCCESS);
}

_IRQL_requires_(PASSIVE_LEVEL)
static ULONG CrosEcConsoleLogQuerySetting(_In_opt_ WDFKEY key, _In_ PCWSTR name, ULONG defaultValue)
{
	if (!key)
		return defaultValue;

	UNICODE_STRING valueName;
	RtlInitUnicodeString(&valueName, name);

	ULONG value;
	if (NT_SUCCESS(WdfRegistryQueryULong(key, &valueName, &value)))
		return value;
	return defaultValue;
}

_IRQL_requires_(PASSIVE_LEVEL)
NTSTATUS CrosEcConsoleLogStart(_In_ PCROSECBUS_CONTEXT pDevice)
{
	PCROSEC_CONSOLE_LOG log = &pDevice->ConsoleLog;
	NTSTATUS status;

	PAGED_CODE();

	// Settings live in HKR\Settings of the device's hardware key
	WDFKEY deviceKey = NULL;
	WDFKEY settingsKey = NULL;
	status = WdfDeviceOpenRegistryKey(pDevice->FxDevice, PLUGPLAY_REGKEY_DEVICE, KEY_READ,
		WDF_NO_OBJECT_ATTRIBUTES, &deviceKey);
	if (NT_SUCCESS(status)) {
		DECLARE_CONST_UNICODE_STRING(settingsName, L"Settings");
		status = WdfRegistryOpenKey(deviceKey, &settingsName, KEY_READ, WDF_NO_OBJECT_ATTRIBUTES, &settingsKey);
		if (!NT_SUCCESS(status))
			settingsKey = NULL;
	}

	ULONG enabled = CrosEcConsoleLogQuerySetting(settingsKey, L"ConsoleLogEnabled", 1);
	log->PollMs = CrosEcConsoleLogQuerySetting(settingsKey, L"ConsoleLogPollMs", CROSEC_CONSOLE_POLL_MS_DEFAULT);
	if (log->PollMs < CROSEC_CONSOLE_POLL_MS_MIN)
		log->PollMs = CROSEC_CONSOLE_POLL_MS_MIN;

	if (settingsKey)
		WdfRegistryClose(settingsKey);
	if (deviceKey)
		WdfRegistryClose(deviceKey);

	if (!enabled) {
		TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_CONSOLELOG, "EC console logging disabled\n");
		return STATUS_SUCCESS;
	}

	if (!ec_command_proto || ec_max_insize == 0)
		return STATUS_NOINTERFACE;

	log->ReadBufSize = ec_max_insize;
	log->ReadBuf = (PCHAR)ExAllocatePool2(POOL_FLAG_NON_PAGED, log->ReadBufSize, CROSECBUS_POOL_TAG);
	if (!log->ReadBuf)
		return STATUS_NO_MEMORY;

	log->DumpedInitial = FALSE;
	log->LineLen = 0;
	KeInitializeEvent(&log->StopEvent, NotificationEvent, FALSE);
	// Start signaled so the thread polls once right away
	KeInitializeEvent(&log->PollEvent, SynchronizationEvent, TRUE);

	OBJECT_ATTRIBUTES threadAttributes;
	InitializeObjectAttributes(&threadAttributes, NULL, OBJ_KERNEL_HANDLE, NULL, NULL);

	HANDLE threadHandle;
	status = PsCreateSystemThread(&threadHandle, THREAD_ALL_ACCESS, &threadAttributes, NULL, NULL,
		CrosEcConsoleLogThread, pDevice);
	if (!NT_SUCCESS(status)) {
		TraceEvents(TRACE_LEVEL_ERROR, TRACE_CONSOLELOG, "PsCreateSystemThread failed %!STATUS!", status);
		ExFreePoolWithTag(log->ReadBuf, CROSECBUS_POOL_TAG);
		log->ReadBuf = NULL;
		return status;
	}

	status = ObReferenceObjectByHandle(threadHandle, THREAD_ALL_ACCESS, *PsThreadType, KernelMode,
		(PVOID*)&log->Thread, NULL);
	if (!NT_SUCCESS(status)) {
		// Can't track the thread; stop it and wait for it before giving up
		KeSetEvent(&log->StopEvent, IO_NO_INCREMENT, FALSE);
		ZwWaitForSingleObject(threadHandle, FALSE, NULL);
		ZwClose(threadHandle);
		log->Thread = NULL;
		ExFreePoolWithTag(log->ReadBuf, CROSECBUS_POOL_TAG);
		log->ReadBuf = NULL;
		return status;
	}
	ZwClose(threadHandle);

	// No-wake timer: doesn't wake the system from Modern Standby, fires on
	// the next wake instead
	log->Timer = ExAllocateTimer(CrosEcConsoleLogTimerCallback, pDevice, EX_TIMER_NO_WAKE);
	if (log->Timer) {
		EXT_SET_PARAMETERS timerParams;
		ExInitializeSetTimerParameters(&timerParams);
		timerParams.NoWakeTolerance = EX_TIMER_UNLIMITED_TOLERANCE;

		LONGLONG period = (LONGLONG)log->PollMs * 10000; // 100ns units
		ExSetTimer(log->Timer, -period, period, &timerParams);
	}
	else {
		TraceEvents(TRACE_LEVEL_ERROR, TRACE_CONSOLELOG, "ExAllocateTimer failed, not polling periodically\n");
	}

	CrosEcConsoleLogSetDevice(pDevice);

	TraceEvents(TRACE_LEVEL_INFORMATION, TRACE_CONSOLELOG, "EC console logging started, polling every %u ms\n",
		log->PollMs);

	return STATUS_SUCCESS;
}

_IRQL_requires_(PASSIVE_LEVEL)
VOID CrosEcConsoleLogStop(_In_ PCROSECBUS_CONTEXT pDevice)
{
	PCROSEC_CONSOLE_LOG log = &pDevice->ConsoleLog;

	PAGED_CODE();

	CrosEcConsoleLogSetDevice(NULL);

	if (log->Timer) {
		// Cancel and wait for a running callback to finish
		ExDeleteTimer(log->Timer, TRUE, TRUE, NULL);
		log->Timer = NULL;
	}

	if (log->Thread) {
		KeSetEvent(&log->StopEvent, IO_NO_INCREMENT, FALSE);
		KeWaitForSingleObject(log->Thread, Executive, KernelMode, FALSE, NULL);
		ObDereferenceObject(log->Thread);
		log->Thread = NULL;
	}

	if (log->ReadBuf) {
		ExFreePoolWithTag(log->ReadBuf, CROSECBUS_POOL_TAG);
		log->ReadBuf = NULL;
	}
}

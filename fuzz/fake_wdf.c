/*
 * Minimal WDF / Ke / Ex behaviour for the user-mode harness, with checks
 * for the driver misusing them (double completion, unbalanced locks).
 */
#include <stdio.h>

#include "fake_wdf.h"
#include "fuzz_input.h"

struct fake_device {
	CROSECBUS_CONTEXT context;
};

struct fake_queue {
	WDFDEVICE device;
	WDF_IO_QUEUE_CONFIG config;
};

struct fake_waitlock {
	int held;
};

static struct fake_device g_device;
static struct fake_queue g_queue;
static struct fake_waitlock g_ec_lock;
static int g_fast_mutexes_held;
static LONGLONG g_now;

void fake_wdf_reset(void) {
	ZeroMemory(&g_device, sizeof(g_device));
	ZeroMemory(&g_queue, sizeof(g_queue));
	g_ec_lock.held = 0;
	g_fast_mutexes_held = 0;
	g_now = 0;

	g_device.context.FxDevice = &g_device;
	g_device.context.EcLock = &g_ec_lock;
}

WDFDEVICE fake_wdf_device(void) {
	return &g_device;
}

WDFQUEUE fake_wdf_queue(void) {
	return &g_queue;
}

EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL* fake_wdf_ioctl_handler(void) {
	return g_queue.config.EvtIoDeviceControl;
}

CROSECBUS_CONTEXT* GetDeviceContext(WDFDEVICE Device) {
	return &Device->context;
}

NTSTATUS WdfIoQueueCreate(WDFDEVICE Device, PWDF_IO_QUEUE_CONFIG Config,
	PWDF_OBJECT_ATTRIBUTES QueueAttributes, WDFQUEUE* Queue) {
	UNREFERENCED_PARAMETER(QueueAttributes);
	g_queue.device = Device;
	g_queue.config = *Config;
	*Queue = &g_queue;
	return STATUS_SUCCESS;
}

WDFDEVICE WdfIoQueueGetDevice(WDFQUEUE Queue) {
	return Queue->device;
}

static NTSTATUS retrieve_buffer(WDFREQUEST Request, size_t length, size_t minimum,
	PVOID* Buffer, size_t* Length) {
	FUZZ_CHECK(!Request->completed);
	if (length == 0 || length < minimum)
		return STATUS_BUFFER_TOO_SMALL;
	*Buffer = Request->buffer;
	if (Length)
		*Length = length;
	return STATUS_SUCCESS;
}

NTSTATUS WdfRequestRetrieveInputBuffer(WDFREQUEST Request, size_t MinimumRequiredLength,
	PVOID* Buffer, size_t* Length) {
	return retrieve_buffer(Request, Request->in_len, MinimumRequiredLength, Buffer, Length);
}

NTSTATUS WdfRequestRetrieveOutputBuffer(WDFREQUEST Request, size_t MinimumRequiredSize,
	PVOID* Buffer, size_t* Length) {
	return retrieve_buffer(Request, Request->out_len, MinimumRequiredSize, Buffer, Length);
}

void WdfRequestSetInformation(WDFREQUEST Request, ULONG_PTR Information) {
	FUZZ_CHECK(!Request->completed);
	Request->information = Information;
}

void WdfRequestComplete(WDFREQUEST Request, NTSTATUS Status) {
	FUZZ_CHECK(!Request->completed);
	FUZZ_CHECK(!g_ec_lock.held);
	FUZZ_CHECK(g_fast_mutexes_held == 0);
	Request->completed = 1;
	Request->status = Status;
}

NTSTATUS WdfWaitLockAcquire(WDFWAITLOCK Lock, PLONGLONG Timeout) {
	FUZZ_CHECK(Timeout == NULL);
	FUZZ_CHECK(!Lock->held);
	Lock->held = 1;
	return STATUS_SUCCESS;
}

void WdfWaitLockRelease(WDFWAITLOCK Lock) {
	FUZZ_CHECK(Lock->held);
	Lock->held = 0;
}

void ExInitializeFastMutex(PFAST_MUTEX Mutex) {
	Mutex->Count = 0;
}

void ExAcquireFastMutex(PFAST_MUTEX Mutex) {
	FUZZ_CHECK(Mutex->Count == 0);
	Mutex->Count = 1;
	g_fast_mutexes_held++;
}

void ExReleaseFastMutex(PFAST_MUTEX Mutex) {
	FUZZ_CHECK(Mutex->Count == 1);
	Mutex->Count = 0;
	g_fast_mutexes_held--;
}

// Simulated clock: delays advance time instantly, so timeouts are cheap
// and deterministic
NTSTATUS KeDelayExecutionThread(KPROCESSOR_MODE WaitMode, BOOLEAN Alertable, PLARGE_INTEGER Interval) {
	UNREFERENCED_PARAMETER(WaitMode);
	UNREFERENCED_PARAMETER(Alertable);
	FUZZ_CHECK(Interval->QuadPart < 0); // driver only uses relative waits
	g_now += -Interval->QuadPart;
	return STATUS_SUCCESS;
}

void KeQuerySystemTimePrecise(PLARGE_INTEGER CurrentTime) {
	CurrentTime->QuadPart = g_now;
}

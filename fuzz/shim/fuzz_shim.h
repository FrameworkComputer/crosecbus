/*
 * Forced-include (/FI) for building the driver sources in user mode.
 *
 * Provides just enough of the WDK (NT types, NTSTATUS codes, port I/O,
 * the few Ke/Ex/Wdf calls the sources use) for comm-lpc.c, comm-mec_lpc.c
 * and userspaceQueue.c to compile unmodified. The real driver.h is skipped
 * by pre-defining its include guard; the driver's own data types come from
 * crosec_types.h so they cannot drift from the shipped driver.
 *
 * Behaviour lives in fake_wdf.c (WDF/Ke/Ex) and fake_ec.c (port I/O).
 */
#pragma once

#define WIN32_LEAN_AND_MEAN
#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS
#include <ntstatus.h>
#include <winioctl.h>
#include <stddef.h>

typedef LONG NTSTATUS;
#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
#define NT_ANALYSIS_ASSUME(expr) ((void)0)
#define PAGED_CODE() ((void)0)

// WPP tracing compiles to nothing
#define TraceEvents(...) ((void)0)
#define Trace(...) ((void)0)

// Kernel types
typedef enum _KPROCESSOR_MODE { KernelMode, UserMode } KPROCESSOR_MODE;
typedef struct _FAST_MUTEX { LONG Count; } FAST_MUTEX, *PFAST_MUTEX;
typedef struct _CALLBACK_OBJECT* PCALLBACK_OBJECT;
typedef struct _ACPI_INTERFACE_STANDARD2 {
	PVOID Context;
	void (*UnregisterForDeviceNotifications)(PVOID Context);
} ACPI_INTERFACE_STANDARD2;

void ExInitializeFastMutex(PFAST_MUTEX Mutex);
void ExAcquireFastMutex(PFAST_MUTEX Mutex);
void ExReleaseFastMutex(PFAST_MUTEX Mutex);
NTSTATUS KeDelayExecutionThread(KPROCESSOR_MODE WaitMode, BOOLEAN Alertable, PLARGE_INTEGER Interval);
void KeQuerySystemTimePrecise(PLARGE_INTEGER CurrentTime);

// Port I/O, routed to the simulated EC
UCHAR READ_PORT_UCHAR(PUCHAR Port);
USHORT READ_PORT_USHORT(PUSHORT Port);
void WRITE_PORT_UCHAR(PUCHAR Port, UCHAR Value);
void WRITE_PORT_USHORT(PUSHORT Port, USHORT Value);

// WDF handles
typedef struct fake_device* WDFDEVICE;
typedef struct fake_queue* WDFQUEUE;
typedef struct fake_request* WDFREQUEST;
typedef struct fake_waitlock* WDFWAITLOCK;
typedef struct fake_interrupt* WDFINTERRUPT;
typedef struct fake_object_attributes* PWDF_OBJECT_ATTRIBUTES;
#define WDF_NO_OBJECT_ATTRIBUTES ((PWDF_OBJECT_ATTRIBUTES)NULL)

typedef VOID EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL(WDFQUEUE Queue, WDFREQUEST Request,
	size_t OutputBufferLength, size_t InputBufferLength, ULONG IoControlCode);
typedef VOID EVT_WDF_IO_QUEUE_IO_STOP(WDFQUEUE Queue, WDFREQUEST Request, ULONG ActionFlags);

typedef enum _WDF_IO_QUEUE_DISPATCH_TYPE { WdfIoQueueDispatchParallel = 2 } WDF_IO_QUEUE_DISPATCH_TYPE;
typedef struct _WDF_IO_QUEUE_CONFIG {
	WDF_IO_QUEUE_DISPATCH_TYPE DispatchType;
	BOOLEAN DefaultQueue;
	EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL* EvtIoDeviceControl;
	EVT_WDF_IO_QUEUE_IO_STOP* EvtIoStop;
} WDF_IO_QUEUE_CONFIG, *PWDF_IO_QUEUE_CONFIG;

static __inline void WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(PWDF_IO_QUEUE_CONFIG Config,
	WDF_IO_QUEUE_DISPATCH_TYPE DispatchType) {
	ZeroMemory(Config, sizeof(*Config));
	Config->DispatchType = DispatchType;
	Config->DefaultQueue = TRUE;
}

NTSTATUS WdfIoQueueCreate(WDFDEVICE Device, PWDF_IO_QUEUE_CONFIG Config,
	PWDF_OBJECT_ATTRIBUTES QueueAttributes, WDFQUEUE* Queue);
WDFDEVICE WdfIoQueueGetDevice(WDFQUEUE Queue);
NTSTATUS WdfRequestRetrieveInputBuffer(WDFREQUEST Request, size_t MinimumRequiredLength,
	PVOID* Buffer, size_t* Length);
NTSTATUS WdfRequestRetrieveOutputBuffer(WDFREQUEST Request, size_t MinimumRequiredSize,
	PVOID* Buffer, size_t* Length);
void WdfRequestSetInformation(WDFREQUEST Request, ULONG_PTR Information);
void WdfRequestComplete(WDFREQUEST Request, NTSTATUS Status);
NTSTATUS WdfWaitLockAcquire(WDFWAITLOCK Lock, PLONGLONG Timeout);
void WdfWaitLockRelease(WDFWAITLOCK Lock);

#include "../../crosecbus/crosec_types.h"

struct _CROSECBUS_CONTEXT* GetDeviceContext(WDFDEVICE Device);

// Skip the real driver.h; everything it would provide is above
#define _CROSECBUS_H_

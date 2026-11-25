#pragma once

#include "fuzz_shim.h"

struct fake_request {
	// METHOD_BUFFERED: one system buffer of max(in_len, out_len) bytes is
	// both the input and the output buffer
	UINT8* buffer;
	size_t in_len;
	size_t out_len;

	ULONG_PTR information;
	NTSTATUS status;
	int completed;
};

// Fresh device context and lock state for each fuzz iteration
void fake_wdf_reset(void);

WDFDEVICE fake_wdf_device(void);

// The queue and callbacks the driver registered via WdfIoQueueCreate
WDFQUEUE fake_wdf_queue(void);
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL* fake_wdf_ioctl_handler(void);

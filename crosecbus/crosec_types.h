#if !defined(_CROSEC_TYPES_H_)
#define _CROSEC_TYPES_H_

//
// Plain data types shared by the driver and the user-mode fuzz harness
// (fuzz/). Only relies on types that driver.h or fuzz/shim/fuzz_shim.h
// provide, so keep WDK-only includes out of this file.
//

//
// String definitions
//

#define DRIVERNAME                 "crosecbus.sys: "

#define CROSECBUS_POOL_TAG            (ULONG) 'CREC'
#define CROSECBUS_HARDWARE_IDS        L"CoolStar\\GOOG0004\0\0"
#define CROSECBUS_HARDWARE_IDS_LENGTH sizeof(CROSECBUS_HARDWARE_IDS)

#define NTDEVICE_NAME_STRING       L"\\Device\\CrosEC"
#define SYMBOLIC_NAME_STRING       L"\\DosDevices\\GOOG0004"

#define true 1
#define false 0

typedef struct _CROSEC_COMMAND {
    UINT32 Version;
    UINT32 Command;
    UINT32 OutSize;
    UINT32 InSize;
    UINT32 Result;
#pragma warning(disable:4200)
    UINT8 Data[];
} CROSEC_COMMAND, *PCROSEC_COMMAND;

typedef struct _CROSECBUS_CONTEXT
{

	//
	// Handle back to the WDFDEVICE
	//

	WDFDEVICE FxDevice;

    UINT32 EcFeatures[2];

    LONG64 KernelAccessesWaiting;
    WDFWAITLOCK EcLock;

    WDFINTERRUPT Interrupt;
    BOOLEAN FoundSyncGPIO;
    PCALLBACK_OBJECT CSButtonsCallback;

    //S0IX Notify
    ACPI_INTERFACE_STANDARD2 S0ixNotifyAcpiInterface;
    BOOLEAN isInS0ix;
    BOOLEAN hostSleepV1;

} CROSECBUS_CONTEXT, *PCROSECBUS_CONTEXT;

#endif

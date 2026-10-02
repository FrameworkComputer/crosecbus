#pragma once

//
// EC console logger: periodically reads the EC console ring buffer and writes
// each line as an event to the Framework-CrosEcBus/Console event log channel.
//

#define CROSEC_CONSOLE_LINE_MAX 512

typedef struct _CROSEC_CONSOLE_LOG {
    PETHREAD Thread;
    KEVENT StopEvent;
    KEVENT PollEvent;
    PEX_TIMER Timer;
    ULONG PollMs;

    BOOLEAN DumpedInitial;

    PCHAR ReadBuf;
    ULONG ReadBufSize;

    CHAR Line[CROSEC_CONSOLE_LINE_MAX];
    ULONG LineLen;
} CROSEC_CONSOLE_LOG, *PCROSEC_CONSOLE_LOG;

struct _CROSECBUS_CONTEXT;

VOID CrosEcConsoleLogRegisterProvider(VOID);
VOID CrosEcConsoleLogUnregisterProvider(VOID);

_IRQL_requires_(PASSIVE_LEVEL)
NTSTATUS CrosEcConsoleLogStart(_In_ struct _CROSECBUS_CONTEXT* pDevice);

_IRQL_requires_(PASSIVE_LEVEL)
VOID CrosEcConsoleLogStop(_In_ struct _CROSECBUS_CONTEXT* pDevice);

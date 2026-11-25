/*
 * libFuzzer target: userspace IOCTLs into crosecbus, against a simulated EC.
 *
 * Input layout (all reads past the end yield zeros):
 *   u8      EC mode (fake_ec_mode)
 *   u8 n    then n bytes of initial EC memory map contents
 *   then up to MAX_OPS operations:
 *     u8    selector: low 2 bits pick XCMD / XCMD / RDMEM / raw code,
 *           bit 7 makes an XCMD header's OutSize/InSize match the buffer lengths
 *     [u32] raw IOCTL code (selector 3 only)
 *     u16   input length, u16 output length (each mod MAX_BUFFER + 1)
 *     u8    fill byte for the system buffer beyond the input (the I/O
 *           manager makes no promises about it)
 *     ...   input buffer bytes
 *   EC replies are consumed from the same stream whenever the EC gets a
 *   command (see fake_ec.c).
 */
#include <stdlib.h>

#include "fake_ec.h"
#include "fake_wdf.h"
#include "comm-host.h"
#include "userspaceQueue.h"

#define MAX_OPS     8
#define MAX_BUFFER  1024

NTSTATUS comm_init_lpc(void);

static void check_xcmd_result(struct fake_request* rq, const CROSEC_COMMAND* sent,
	UINT32 ec_count_before) {
	if (!NT_SUCCESS(rq->status))
		return;

	const fake_ec_request* ec = fake_ec_last_request();
	FUZZ_CHECK(rq->in_len >= sizeof(CROSEC_COMMAND));
	FUZZ_CHECK(ec->count == ec_count_before + 1);
	FUZZ_CHECK(ec->command == (UINT16)sent->Command);
	FUZZ_CHECK(ec->data_len == sent->OutSize);
	FUZZ_CHECK(rq->information >= sizeof(CROSEC_COMMAND));
	FUZZ_CHECK(rq->information - sizeof(CROSEC_COMMAND) <= sent->InSize);
}

static void check_rdmem_result(struct fake_request* rq) {
	if (!NT_SUCCESS(rq->status))
		return;

	const CROSEC_READMEM* rs = (const CROSEC_READMEM*)rq->buffer;
	FUZZ_CHECK(rq->information == sizeof(CROSEC_READMEM));
	FUZZ_CHECK(rs->bytes <= sizeof(rs->buffer));
}

static void run_op(fuzz_input* in) {
	UINT8 selector = fi_u8(in);
	ULONG code;
	switch (selector & 3) {
	case 0:
	case 1:
		code = IOCTL_CROSEC_XCMD;
		break;
	case 2:
		code = IOCTL_CROSEC_RDMEM;
		break;
	default:
		code = fi_u32(in);
		break;
	}

	struct fake_request rq = { 0 };
	rq.in_len = fi_u16(in) % (MAX_BUFFER + 1);
	rq.out_len = fi_u16(in) % (MAX_BUFFER + 1);

	UINT8 uninitialized = fi_u8(in);

	// Exactly-sized allocation so ASan flags any access past the system buffer
	size_t buffer_len = max(rq.in_len, rq.out_len);
	rq.buffer = buffer_len ? malloc(buffer_len) : NULL;
	if (buffer_len) {
		fi_bytes(in, rq.buffer, rq.in_len);
		memset(rq.buffer + rq.in_len, uninitialized, buffer_len - rq.in_len);
	}

	CROSEC_COMMAND sent = { 0 };
	if (code == IOCTL_CROSEC_XCMD && rq.in_len >= sizeof(CROSEC_COMMAND)) {
		PCROSEC_COMMAND cmd = (PCROSEC_COMMAND)rq.buffer;
		if (selector & 0x80) {
			cmd->OutSize = (UINT32)(rq.in_len - sizeof(CROSEC_COMMAND));
			cmd->InSize = rq.out_len >= sizeof(CROSEC_COMMAND) ?
				(UINT32)(rq.out_len - sizeof(CROSEC_COMMAND)) : 0;
		}
		sent = *cmd;
	}

	UINT32 ec_count_before = fake_ec_last_request()->count;

	fake_wdf_ioctl_handler()(fake_wdf_queue(), &rq, rq.out_len, rq.in_len, code);

#ifdef FUZZ_VERBOSE
	fprintf(stderr, "ioctl 0x%08lx in %zu out %zu -> status 0x%08lx info %zu (EC commands: %u)\n",
		code, rq.in_len, rq.out_len, (unsigned long)rq.status, (size_t)rq.information,
		fake_ec_last_request()->count);
#endif

	FUZZ_CHECK(rq.completed);
	if (NT_SUCCESS(rq.status))
		FUZZ_CHECK(rq.information <= rq.out_len);
	if (code == IOCTL_CROSEC_XCMD)
		check_xcmd_result(&rq, &sent, ec_count_before);
	else if (code == IOCTL_CROSEC_RDMEM)
		check_rdmem_result(&rq);

	free(rq.buffer);
}

int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
	fuzz_input in = { data, size };

	fake_ec_mode mode = (fake_ec_mode)(fi_u8(&in) % FAKE_EC_MODE_COUNT);
	UINT8 memmap[EC_MEMMAP_SIZE] = { 0 };
	UINT8 memmap_len = fi_u8(&in);
	fi_bytes(&in, memmap, min(memmap_len, EC_MEMMAP_SIZE));

	fake_wdf_reset();
	fake_ec_reset(mode, &in, memmap);

	if (!NT_SUCCESS(comm_init_lpc()))
		return 0;
	FUZZ_CHECK(NT_SUCCESS(CrosECQueueInitialize(fake_wdf_device())));

	for (int op = 0; op < MAX_OPS && in.size; op++)
		run_op(&in);

	return 0;
}

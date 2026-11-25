#pragma once

#include "fuzz_input.h"

typedef enum fake_ec_mode {
	FAKE_EC_LPC_V2,   // legacy args/params protocol
	FAKE_EC_LPC_V3,   // packet protocol over plain LPC ports
	FAKE_EC_MEC,      // packet protocol over the MEC EMI window (Framework)
	FAKE_EC_ABSENT,   // ports float high, no EC
	FAKE_EC_MODE_COUNT
} fake_ec_mode;

// What the EC last saw from the host
typedef struct fake_ec_request {
	UINT32 count;     // commands received since reset
	UINT16 command;
	UINT8 version;
	UINT16 data_len;
} fake_ec_request;

// memmap: EC_MEMMAP_SIZE bytes of initial memory-map contents. The EC
// consumes its replies to commands lazily from `script`.
void fake_ec_reset(fake_ec_mode mode, fuzz_input* script, const UINT8* memmap);

const fake_ec_request* fake_ec_last_request(void);

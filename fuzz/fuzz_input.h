/*
 * Sequential reader over the fuzzer's input. Once the input runs out every
 * read returns zeros, so the harness never has to bail out mid-operation.
 */
#pragma once

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct fuzz_input {
	const uint8_t* data;
	size_t size;
} fuzz_input;

static __inline size_t fi_bytes(fuzz_input* in, void* dst, size_t n) {
	size_t take = n < in->size ? n : in->size;
	memcpy(dst, in->data, take);
	memset((uint8_t*)dst + take, 0, n - take);
	in->data += take;
	in->size -= take;
	return take;
}

static __inline uint8_t fi_u8(fuzz_input* in) {
	uint8_t v;
	fi_bytes(in, &v, sizeof(v));
	return v;
}

static __inline uint16_t fi_u16(fuzz_input* in) {
	uint16_t v;
	fi_bytes(in, &v, sizeof(v));
	return v;
}

static __inline uint32_t fi_u32(fuzz_input* in) {
	uint32_t v;
	fi_bytes(in, &v, sizeof(v));
	return v;
}

// Harness invariant violated: crash so libFuzzer saves the input
#define FUZZ_CHECK(cond)                                                    \
	do {                                                                    \
		if (!(cond)) {                                                      \
			fprintf(stderr, "FUZZ_CHECK failed: %s (%s:%d)\n", #cond,       \
				__FILE__, __LINE__);                                        \
			abort();                                                        \
		}                                                                   \
	} while (0)

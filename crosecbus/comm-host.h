#ifndef __COMM_HOST_H__
#define __COMM_HOST_H__

#include "ec_commands.h"

/* ec_command return value for non-success result from EC */
#define EECRESULT 1000

typedef struct lpc_driver_ops {
	int(*read)(unsigned int offset, unsigned int length, UINT8* dest);
	int(*write)(unsigned int offset, unsigned int length, const UINT8* dest);
} lpc_driver_ops;

extern UINT32 ec_max_outsize, ec_max_insize;

extern lpc_driver_ops ec_lpc_ops;

extern int (*ec_command_proto)(UINT16 command, UINT8 version,
	const void* outdata, int outsize, /* to EC */
	void* indata, int insize);        /* from EC */

/**
 * Return the content of the EC information area mapped as "memory".
 * The offsets are defined by the EC_MEMMAP_ constants. Returns the number
 * of bytes read, or negative on error. Specifying bytes=0 will read a
 * string (always including the trailing '\0').
 */
extern int (*ec_readmem)(int offset, int bytes, void* dest);

static __inline const char* ec_result_to_string(int res) {
	switch (res) {
	case EC_RES_SUCCESS:
		return "SUCCESS";
	case EC_RES_INVALID_COMMAND:
		return "INVALID_COMMAND";
	case EC_RES_ERROR:
		return "ERROR";
	case EC_RES_INVALID_PARAM:
		return "INVALID_PARAM";
	case EC_RES_ACCESS_DENIED:
		return "ACCESS_DENIED";
	case EC_RES_INVALID_RESPONSE:
		return "INVALID_RESPONSE";
	case EC_RES_INVALID_VERSION:
		return "INVALID_VERSION";
	case EC_RES_INVALID_CHECKSUM:
		return "INVALID_CHECKSUM";
	case EC_RES_IN_PROGRESS:
		return "IN_PROGRESS";
	case EC_RES_UNAVAILABLE:
		return "UNAVAILABLE";
	case EC_RES_TIMEOUT:
		return "TIMEOUT";
	case EC_RES_OVERFLOW:
		return "OVERFLOW";
	case EC_RES_INVALID_HEADER:
		return "INVALID_HEADER";
	case EC_RES_REQUEST_TRUNCATED:
		return "REQUEST_TRUNCATED";
	case EC_RES_RESPONSE_TOO_BIG:
		return "RESPONSE_TOO_BIG";
	case EC_RES_BUS_ERROR:
		return "BUS_ERROR";
	case EC_RES_BUSY:
		return "BUSY";
	case EC_RES_INVALID_HEADER_VERSION:
		return "INVALID_HEADER_VERSION";
	case EC_RES_INVALID_HEADER_CRC:
		return "INVALID_HEADER_CRC";
	case EC_RES_INVALID_DATA_CRC:
		return "INVALID_DATA_CRC";
	case EC_RES_DUP_UNAVAILABLE:
		return "DUP_UNAVAILABLE";
	default:
		return "Unknown";
	}
}

#endif
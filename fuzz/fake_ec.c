/*
 * Simulated Chrome EC behind the port I/O calls (READ_PORT_* / WRITE_PORT_*).
 *
 * The EC parses and validates what the driver sends, then answers with
 * whatever the fuzz input says: result code, length, payload, and optional
 * protocol violations (bad checksum, bad version, never going idle, ...).
 *
 * Port map:
 *   0x200      EC_LPC_ADDR_HOST_DATA  result code of the last command
 *   0x204      EC_LPC_ADDR_HOST_CMD   write: start command, read: status
 *   0x800-     host args / packet region
 *   0xE00-     memory map (EC_LPC_ADDR_MEMMAP)
 * In MEC mode, 0x800-0x807 are EMI registers instead, and the host
 * packet and memory map live in EC RAM reached through them.
 */
#include "fake_ec.h"
#include "comm-host.h"

// Script flags, first byte of each reply
#define REPLY_BUSY_POLLS_MASK  0x03  // report busy for this many status reads
#define REPLY_HANG             0x04  // stay busy forever (driver times out)
#define REPLY_BAD_CHECKSUM     0x08
#define REPLY_BAD_VERSION      0x10  // v3: wrong struct_version, v2: no TO_HOST flag
#define REPLY_RESERVED_SET     0x20  // v3: nonzero reserved field
#define REPLY_LONG_LEN         0x40  // data length has a high byte

#define EMI_BASE      EC_HOST_CMD_REGION0
#define EMI_RAM_SIZE  0x800          // EC RAM visible at logical 0x800-0xFFF

static fake_ec_mode g_mode;
static fuzz_input* g_script;
static UINT8 g_ports[0x10000];
static UINT8 g_emi_ram[EMI_RAM_SIZE];
static UINT16 g_emi_address;         // EMI address register, low 2 bits = access mode
static UINT32 g_busy_polls;
static fake_ec_request g_last;

// Host-interface memory as the EC sees it
static UINT8* host_mem(unsigned int port) {
	if (g_mode == FAKE_EC_MEC && port >= EMI_BASE && port < EMI_BASE + EMI_RAM_SIZE)
		return &g_emi_ram[port - EMI_BASE];
	return &g_ports[port & 0xFFFF];
}

static int is_emi_register(unsigned int port) {
	return g_mode == FAKE_EC_MEC && port >= EMI_BASE && port < EMI_BASE + 8;
}

static UINT8* emi_data(unsigned int port) {
	return &g_emi_ram[((g_emi_address & 0xFFFC) + (port - (EMI_BASE + 4))) % EMI_RAM_SIZE];
}

static void emi_after_data_access(unsigned int port) {
	// 32-bit auto-increment mode advances after touching data byte 3
	if ((g_emi_address & 3) == 3 && port == EMI_BASE + 7)
		g_emi_address = (UINT16)(g_emi_address + 4);
}

static void write_reply_payload(unsigned int port, UINT16 len, unsigned int capacity) {
	for (unsigned int i = 0; i < len && i < capacity; i++)
		*host_mem(port + i) = fi_u8(g_script);
}

static int is_blocked_command(UINT16 command) {
	// userspaceQueue.c must never let these through to the EC
	return command == 0x0012 /* FLASH_WRITE */ || command == 0x0013 /* FLASH_ERASE */ ||
		command == 0x0015 /* FLASH_PROTECT */ || command == 0x0110 /* USB_PD_FW_UPDATE */;
}

static void record_request(UINT16 command, UINT8 version, UINT16 data_len) {
	FUZZ_CHECK(!is_blocked_command(command));
	g_last.count++;
	g_last.command = command;
	g_last.version = version;
	g_last.data_len = data_len;
}

static void handle_command_v2(UINT8 command) {
	struct ec_lpc_host_args* args = (struct ec_lpc_host_args*)host_mem(EC_LPC_ADDR_HOST_ARGS);
	UINT8* params = host_mem(EC_LPC_ADDR_HOST_PARAM);

	FUZZ_CHECK(args->flags == EC_HOST_ARGS_FLAG_FROM_HOST);
	FUZZ_CHECK(args->data_size <= EC_PROTO2_MAX_PARAM_SIZE);
	UINT8 csum = (UINT8)(command + args->flags + args->command_version + args->data_size);
	for (int i = 0; i < args->data_size; i++)
		csum += params[i];
	FUZZ_CHECK(csum == args->checksum);
	record_request(command, args->command_version, args->data_size);

	UINT8 flags = fi_u8(g_script);
	UINT8 result = fi_u8(g_script);
	UINT8 len = fi_u8(g_script);

	g_busy_polls = (flags & REPLY_HANG) ? MAXUINT32 : (flags & REPLY_BUSY_POLLS_MASK);
	g_ports[EC_LPC_ADDR_HOST_DATA] = result;
	if (result)
		return;

	args->flags = (flags & REPLY_BAD_VERSION) ? 0 : EC_HOST_ARGS_FLAG_TO_HOST;
	args->data_size = len;
	write_reply_payload(EC_LPC_ADDR_HOST_PARAM, len, EC_PROTO2_MAX_PARAM_SIZE);
	csum = (UINT8)(command + args->flags + args->command_version + args->data_size);
	for (int i = 0; i < len; i++)
		csum += params[i];
	args->checksum = (UINT8)(csum + !!(flags & REPLY_BAD_CHECKSUM));
}

static void handle_command_v3(UINT8 command) {
	struct ec_host_request rq;
	struct ec_host_response rs;
	UINT8* packet = host_mem(EC_LPC_ADDR_HOST_PACKET);

	FUZZ_CHECK(command == EC_COMMAND_PROTOCOL_3);
	memcpy(&rq, packet, sizeof(rq));
	FUZZ_CHECK(rq.struct_version == EC_HOST_REQUEST_VERSION);
	FUZZ_CHECK(rq.reserved == 0);
	FUZZ_CHECK(sizeof(rq) + rq.data_len <= EC_LPC_HOST_PACKET_SIZE);
	UINT8 csum = 0;
	for (size_t i = 0; i < sizeof(rq) + rq.data_len; i++)
		csum += packet[i];
	FUZZ_CHECK(csum == 0);
	record_request(rq.command, rq.command_version, rq.data_len);

	UINT8 flags = fi_u8(g_script);
	UINT8 result = fi_u8(g_script);
	UINT16 len = fi_u8(g_script);
	if (flags & REPLY_LONG_LEN)
		len |= (UINT16)(fi_u8(g_script) << 8);

	g_busy_polls = (flags & REPLY_HANG) ? MAXUINT32 : (flags & REPLY_BUSY_POLLS_MASK);
	g_ports[EC_LPC_ADDR_HOST_DATA] = result;
	if (result)
		return;

	rs.struct_version = (flags & REPLY_BAD_VERSION) ? EC_HOST_RESPONSE_VERSION + 1 : EC_HOST_RESPONSE_VERSION;
	rs.checksum = 0;
	rs.result = 0;
	rs.data_len = len;
	rs.reserved = (flags & REPLY_RESERVED_SET) ? 1 : 0;
	memcpy(packet, &rs, sizeof(rs));
	write_reply_payload(EC_LPC_ADDR_HOST_PACKET + sizeof(rs), len,
		EC_LPC_HOST_PACKET_SIZE - sizeof(rs));

	csum = 0;
	for (size_t i = 0; i < sizeof(rs) + min(len, EC_LPC_HOST_PACKET_SIZE - sizeof(rs)); i++)
		csum += packet[i];
	packet[offsetof(struct ec_host_response, checksum)] =
		(UINT8)(-csum + !!(flags & REPLY_BAD_CHECKSUM));
}

static UINT8 port_read(unsigned int port) {
	if (g_mode == FAKE_EC_ABSENT)
		return 0xFF;

	if (port == EC_LPC_ADDR_HOST_CMD) {
		if (g_busy_polls) {
			if (g_busy_polls != MAXUINT32)
				g_busy_polls--;
			return EC_LPC_STATUS_PROCESSING;
		}
		return 0;
	}

	if (is_emi_register(port)) {
		if (port == EMI_BASE + 2)
			return (UINT8)g_emi_address;
		if (port == EMI_BASE + 3)
			return (UINT8)(g_emi_address >> 8);
		if (port >= EMI_BASE + 4) {
			UINT8 v = *emi_data(port);
			emi_after_data_access(port);
			return v;
		}
	}

	return g_ports[port & 0xFFFF];
}

static void port_write(unsigned int port, UINT8 value) {
	if (g_mode == FAKE_EC_ABSENT)
		return;

	if (port == EC_LPC_ADDR_HOST_CMD) {
		if (g_mode == FAKE_EC_LPC_V2)
			handle_command_v2(value);
		else
			handle_command_v3(value);
		return;
	}

	if (is_emi_register(port)) {
		if (port == EMI_BASE + 2) {
			g_emi_address = (g_emi_address & 0xFF00) | value;
			return;
		}
		if (port == EMI_BASE + 3) {
			g_emi_address = (UINT16)((g_emi_address & 0x00FF) | (value << 8));
			return;
		}
		if (port >= EMI_BASE + 4) {
			*emi_data(port) = value;
			emi_after_data_access(port);
			return;
		}
	}

	g_ports[port & 0xFFFF] = value;
}

UCHAR READ_PORT_UCHAR(PUCHAR Port) {
	return port_read((unsigned int)(ULONG_PTR)Port);
}

USHORT READ_PORT_USHORT(PUSHORT Port) {
	unsigned int port = (unsigned int)(ULONG_PTR)Port;
	UINT8 lo = port_read(port);
	UINT8 hi = port_read(port + 1);
	return (USHORT)(lo | (hi << 8));
}

void WRITE_PORT_UCHAR(PUCHAR Port, UCHAR Value) {
	port_write((unsigned int)(ULONG_PTR)Port, Value);
}

void WRITE_PORT_USHORT(PUSHORT Port, USHORT Value) {
	unsigned int port = (unsigned int)(ULONG_PTR)Port;
	port_write(port, (UINT8)Value);
	port_write(port + 1, (UINT8)(Value >> 8));
}

void fake_ec_reset(fake_ec_mode mode, fuzz_input* script, const UINT8* memmap) {
	g_mode = mode;
	g_script = script;
	ZeroMemory(g_ports, sizeof(g_ports));
	ZeroMemory(g_emi_ram, sizeof(g_emi_ram));
	ZeroMemory(&g_last, sizeof(g_last));
	g_emi_address = 0;
	g_busy_polls = 0;

	UINT8* map = host_mem(EC_LPC_ADDR_MEMMAP);
	memcpy(map, memmap, EC_MEMMAP_SIZE);
	map[EC_MEMMAP_ID] = 'E';
	map[EC_MEMMAP_ID + 1] = 'C';
	map[EC_MEMMAP_HOST_CMD_FLAGS] = (mode == FAKE_EC_LPC_V2) ?
		EC_HOST_CMD_FLAG_LPC_ARGS_SUPPORTED :
		EC_HOST_CMD_FLAG_LPC_ARGS_SUPPORTED | EC_HOST_CMD_FLAG_VERSION_3;
}

const fake_ec_request* fake_ec_last_request(void) {
	return &g_last;
}

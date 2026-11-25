"""Regenerates fuzz/corpus/: well-formed inputs for fuzz_ioctl (layout in fuzz_ioctl.c)."""
import pathlib
import struct

LPC_V2, LPC_V3, MEC = 0, 1, 2
MODES = {"lpc2": LPC_V2, "lpc3": LPC_V3, "mec": MEC}

CROSEC_COMMAND_SIZE = 20
CROSEC_READMEM_SIZE = 264  # 8 + 255, padded to ULONG alignment

XCMD, RDMEM = 0x00, 0x02
CONSISTENT_SIZES = 0x80

EC_CMD_HELLO = 0x01
EC_CMD_GET_VERSION = 0x02
EC_CMD_GET_FEATURES = 0x0D
EC_RES_INVALID_COMMAND = 1


def header(mode):
    return struct.pack("<BB", mode, 0)  # mode, empty memmap override


def xcmd(command, out_data, in_size, version=0):
    buf = struct.pack("<IIIII", version, command, len(out_data), in_size, 0) + out_data
    return struct.pack("<BHHB", XCMD | CONSISTENT_SIZES, len(buf), CROSEC_COMMAND_SIZE + in_size, 0) + buf


def rdmem(offset, nbytes):
    buf = struct.pack("<II", offset, nbytes).ljust(CROSEC_READMEM_SIZE, b"\0")
    return struct.pack("<BHHB", RDMEM, CROSEC_READMEM_SIZE, CROSEC_READMEM_SIZE, 0) + buf


def reply(payload, result=0, flags=0):
    if result:
        return struct.pack("<BBB", flags, result, 0)
    return struct.pack("<BBB", flags, result, len(payload)) + payload


def main():
    out = pathlib.Path(__file__).parent / "corpus"
    out.mkdir(exist_ok=True)

    version = b"seed-ro".ljust(32, b"\0") + b"seed-rw".ljust(32, b"\0") + bytes(32) + struct.pack("<I", 1)
    seeds = {
        "hello": xcmd(EC_CMD_HELLO, struct.pack("<I", 0xA0B0C0D0), 4) + reply(struct.pack("<I", 0xA1B2C3D4)),
        "get_version": xcmd(EC_CMD_GET_VERSION, b"", len(version)) + reply(version),
        "get_features": xcmd(EC_CMD_GET_FEATURES, b"", 8) + reply(struct.pack("<II", 0xFFFF, 0)),
        "ec_error": xcmd(0xFE, b"", 0) + reply(b"", result=EC_RES_INVALID_COMMAND),
        "rdmem_id": rdmem(0x20, 2),
        "rdmem_string": rdmem(0, 0),
    }
    seeds["sequence"] = seeds["hello"] + seeds["rdmem_id"] + seeds["get_features"]

    for mode_name, mode in MODES.items():
        for name, body in seeds.items():
            (out / f"{mode_name}_{name}").write_bytes(header(mode) + body)


if __name__ == "__main__":
    main()

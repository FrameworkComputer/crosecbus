//! Host-side tests against a simulated EC that implements the LPC v2/v3
//! host command interfaces and the MEC EMI window at the port level.

use std::cell::{Cell, RefCell};

use super::*;
use crate::commands::{EcResponseGetFeatures, EcResponseGetVersion, Pod};

type Handler = Box<dyn Fn(u16, u8, &[u8]) -> (u8, Vec<u8>)>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bus {
    Absent,
    Lpc { flags: u8 },
    Mec,
}

struct SimEc {
    bus: Bus,
    /// Raw LPC ports. With `Bus::Mec`, 0x800..0x808 are the EMI registers
    /// and the rest of the region isn't decoded.
    ports: RefCell<Vec<u8>>,
    /// MEC EC RAM, offset 0 corresponds to port 0x800.
    ram: RefCell<Vec<u8>>,
    emi_addr: Cell<u16>,
    emi_mode: Cell<u16>,
    now: Cell<u64>,
    busy_forever: bool,
    corrupt_response: Cell<bool>,
    handler: Handler,
    lock_depth: Cell<u32>,
    locks_taken: Cell<u32>,
}

struct Guard<'a>(&'a Cell<u32>);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl SimEc {
    fn new(bus: Bus, handler: Handler) -> Self {
        let sim = Self {
            bus,
            ports: RefCell::new(vec![0; 0x1000]),
            ram: RefCell::new(vec![0; 0x700]),
            emi_addr: Cell::new(0),
            emi_mode: Cell::new(0),
            now: Cell::new(0),
            busy_forever: false,
            corrupt_response: Cell::new(false),
            handler,
            lock_depth: Cell::new(0),
            locks_taken: Cell::new(0),
        };
        let memmap = usize::from(EC_LPC_ADDR_MEMMAP);
        let id = memmap + usize::from(EC_MEMMAP_ID);
        let flags_at = memmap + usize::from(EC_MEMMAP_HOST_CMD_FLAGS);
        match bus {
            Bus::Absent => {}
            Bus::Lpc { flags } => {
                let mut p = sim.ports.borrow_mut();
                p[id] = b'E';
                p[id + 1] = b'C';
                p[flags_at] = flags;
                // A string in the memory map, for read_mem tests.
                p[memmap + 0x40..memmap + 0x46].copy_from_slice(b"hello\0");
            }
            Bus::Mec => {
                let mut r = sim.ram.borrow_mut();
                r[id - 0x800] = b'E';
                r[id + 1 - 0x800] = b'C';
                r[flags_at - 0x800] = EC_HOST_CMD_FLAG_VERSION_3;
                r[memmap + 0x41 - 0x800..memmap + 0x47 - 0x800].copy_from_slice(b"hello\0");
            }
        }
        sim
    }

    /// Host command region as the EC sees it.
    fn region_get(&self, port: usize) -> u8 {
        if self.bus == Bus::Mec {
            self.ram.borrow()[port - 0x800]
        } else {
            self.ports.borrow()[port]
        }
    }

    fn region_set(&self, port: usize, v: u8) {
        if self.bus == Bus::Mec {
            self.ram.borrow_mut()[port - 0x800] = v;
        } else {
            self.ports.borrow_mut()[port] = v;
        }
    }

    fn is_emi(&self, port: u16) -> bool {
        self.bus == Bus::Mec && (0x800..0x808).contains(&port)
    }

    fn emi_ram_index(&self, port: u16) -> usize {
        usize::from(self.emi_addr.get()) + usize::from(port - MEC_EMI_EC_DATA_B0)
    }

    fn emi_after_access(&self, port: u16, width: u16) {
        // Autoincrement triggers on accessing DATA_B3.
        if self.emi_mode.get() == MecAccess::LongAutoIncrement as u16
            && port + width > MEC_EMI_EC_DATA_B0 + 3
        {
            self.emi_addr.set(self.emi_addr.get() + 4);
        }
    }

    fn run_command(&self, cmd_byte: u8) {
        let result;
        if cmd_byte == EC_COMMAND_PROTOCOL_3 {
            let hdr: Vec<u8> = (0x800..0x808).map(|p| self.region_get(p)).collect();
            let len = usize::from(u16::from_le_bytes([hdr[6], hdr[7]]));
            let data: Vec<u8> = (0x808..0x808 + len).map(|p| self.region_get(p)).collect();
            if checksum(&hdr).wrapping_add(checksum(&data)) != 0 || hdr[0] != 3 {
                result = res::INVALID_CHECKSUM;
            } else {
                let command = u16::from_le_bytes([hdr[2], hdr[3]]);
                let (r, resp) = (self.handler)(command, hdr[4], &data);
                result = r;
                #[allow(clippy::cast_possible_truncation)]
                let rlen = (resp.len() as u16).to_le_bytes();
                let mut rs = [3, 0, r, 0, rlen[0], rlen[1], 0, 0];
                rs[1] = checksum(&rs).wrapping_add(checksum(&resp)).wrapping_neg();
                if self.corrupt_response.get() {
                    rs[1] = rs[1].wrapping_add(1);
                }
                for (i, b) in rs.iter().chain(&resp).enumerate() {
                    self.region_set(0x800 + i, *b);
                }
            }
        } else {
            let args: Vec<u8> = (0x800..0x804).map(|p| self.region_get(p)).collect();
            let len = usize::from(args[2]);
            let data: Vec<u8> = (0x804..0x804 + len).map(|p| self.region_get(p)).collect();
            let expect = cmd_byte
                .wrapping_add(args[0])
                .wrapping_add(args[1])
                .wrapping_add(args[2])
                .wrapping_add(checksum(&data));
            if expect == args[3] {
                let (r, resp) = (self.handler)(u16::from(cmd_byte), args[1], &data);
                result = r;
                #[allow(clippy::cast_possible_truncation)]
                let rlen = resp.len() as u8;
                let flags = EC_HOST_ARGS_FLAG_TO_HOST;
                let mut csum = cmd_byte
                    .wrapping_add(flags)
                    .wrapping_add(args[1])
                    .wrapping_add(rlen)
                    .wrapping_add(checksum(&resp));
                if self.corrupt_response.get() {
                    csum = csum.wrapping_add(1);
                }
                for (i, b) in [flags, args[1], rlen, csum].iter().enumerate() {
                    self.region_set(0x800 + i, *b);
                }
                for (i, b) in resp.iter().enumerate() {
                    self.region_set(0x804 + i, *b);
                }
            } else {
                result = res::INVALID_CHECKSUM;
            }
        }
        self.ports.borrow_mut()[usize::from(EC_LPC_ADDR_HOST_DATA)] = result;
    }
}

impl Hal for SimEc {
    type Guard<'a> = Guard<'a>;

    fn inb(&self, port: u16) -> u8 {
        match self.bus {
            Bus::Absent => return 0xFF,
            Bus::Mec if self.is_emi(port) => {
                if port >= MEC_EMI_EC_DATA_B0 {
                    assert!(self.lock_depth.get() > 0, "EMI access without lock");
                    let v = self.ram.borrow()[self.emi_ram_index(port)];
                    self.emi_after_access(port, 1);
                    return v;
                }
                return 0;
            }
            Bus::Mec if (0x808..0xF00).contains(&port) => return 0xFF,
            _ => {}
        }
        if port == EC_LPC_ADDR_HOST_CMD {
            return if self.busy_forever {
                EC_LPC_STATUS_PROCESSING
            } else {
                0
            };
        }
        self.ports.borrow()[usize::from(port)]
    }

    fn outb(&self, port: u16, value: u8) {
        if self.is_emi(port) {
            assert!(port >= MEC_EMI_EC_DATA_B0, "byte write to EMI address reg");
            assert!(self.lock_depth.get() > 0, "EMI access without lock");
            let i = self.emi_ram_index(port);
            self.ram.borrow_mut()[i] = value;
            self.emi_after_access(port, 1);
            return;
        }
        if port == EC_LPC_ADDR_HOST_CMD {
            if !self.busy_forever {
                self.run_command(value);
            }
            return;
        }
        self.ports.borrow_mut()[usize::from(port)] = value;
    }

    fn inw(&self, port: u16) -> u16 {
        if self.bus != Bus::Mec {
            return u16::from_le_bytes([self.inb(port), self.inb(port + 1)]);
        }
        assert!(self.is_emi(port) && port >= MEC_EMI_EC_DATA_B0 && port.is_multiple_of(2));
        assert!(self.lock_depth.get() > 0, "EMI access without lock");
        let i = self.emi_ram_index(port);
        let v = {
            let ram = self.ram.borrow();
            u16::from_le_bytes([ram[i], ram[i + 1]])
        };
        self.emi_after_access(port, 2);
        v
    }

    fn outw(&self, port: u16, value: u16) {
        if self.bus != Bus::Mec {
            // The probe pokes the EMI address register before it knows
            // whether the EC is a MEC; on LPC that's just a port write.
            let [lo, hi] = value.to_le_bytes();
            self.outb(port, lo);
            self.outb(port + 1, hi);
            return;
        }
        assert!(self.is_emi(port) && port.is_multiple_of(2));
        assert!(self.lock_depth.get() > 0, "EMI access without lock");
        if port == MEC_EMI_EC_ADDRESS_B0 {
            self.emi_addr.set(value & 0xFFFC);
            self.emi_mode.set(value & 0x3);
            return;
        }
        let i = self.emi_ram_index(port);
        self.ram.borrow_mut()[i..i + 2].copy_from_slice(&value.to_le_bytes());
        self.emi_after_access(port, 2);
    }

    fn delay_us(&self, us: u32) {
        self.now.set(self.now.get() + u64::from(us));
    }

    fn now_us(&self) -> u64 {
        self.now.get()
    }

    fn lock_mec(&self) -> Guard<'_> {
        assert_eq!(self.lock_depth.get(), 0, "MEC lock is not reentrant");
        self.lock_depth.set(1);
        self.locks_taken.set(self.locks_taken.get() + 1);
        Guard(&self.lock_depth)
    }

    fn log(&self, args: fmt::Arguments<'_>) {
        println!("{args}");
    }
}

/// Echoes the request reversed, so tests can check both directions.
fn reverse_handler() -> Handler {
    Box::new(|_cmd, _ver, data| (0, data.iter().rev().copied().collect()))
}

fn version_handler() -> Handler {
    Box::new(|cmd, _ver, _data| match cmd {
        commands::EC_CMD_GET_VERSION => {
            let mut v = vec![0u8; 100];
            v[..9].copy_from_slice(b"ro-1.2.3\0");
            v[32..41].copy_from_slice(b"rw-4.5.6\0");
            (0, v)
        }
        commands::EC_CMD_GET_FEATURES => (0, vec![0x01, 0x00, 0x00, 0x80, 0x02, 0, 0, 0]),
        _ => (res::INVALID_COMMAND, vec![]),
    })
}

fn all_bus_kinds() -> [Bus; 3] {
    [
        Bus::Lpc {
            flags: EC_HOST_CMD_FLAG_LPC_ARGS_SUPPORTED,
        },
        Bus::Lpc {
            flags: EC_HOST_CMD_FLAG_VERSION_3 | EC_HOST_CMD_FLAG_LPC_ARGS_SUPPORTED,
        },
        Bus::Mec,
    ]
}

#[test]
fn probe_absent() {
    let r = Ec::probe(SimEc::new(Bus::Absent, reverse_handler()));
    assert_eq!(r.err(), Some(ProbeError::NotPresent));
}

#[test]
fn probe_unsupported_protocol() {
    let r = Ec::probe(SimEc::new(Bus::Lpc { flags: 0 }, reverse_handler()));
    assert_eq!(r.err(), Some(ProbeError::UnsupportedProtocol));
}

#[test]
fn probe_missing_memmap() {
    let sim = SimEc::new(Bus::Lpc { flags: 1 }, reverse_handler());
    sim.ports.borrow_mut()[0xE20] = 0;
    assert_eq!(Ec::probe(sim).err(), Some(ProbeError::NoMemoryMap));
}

#[test]
fn probe_selects_protocol() {
    let ec = Ec::probe(SimEc::new(all_bus_kinds()[0], reverse_handler())).unwrap();
    assert_eq!((ec.protocol(), ec.is_mec()), (Protocol::V2, false));
    assert_eq!((ec.max_insize(), ec.max_outsize()), (0xFC, 0xFC));

    let ec = Ec::probe(SimEc::new(all_bus_kinds()[1], reverse_handler())).unwrap();
    assert_eq!((ec.protocol(), ec.is_mec()), (Protocol::V3, false));
    assert_eq!((ec.max_insize(), ec.max_outsize()), (0xF8, 0xF8));

    let ec = Ec::probe(SimEc::new(Bus::Mec, reverse_handler())).unwrap();
    assert_eq!((ec.protocol(), ec.is_mec()), (Protocol::V3, true));
    assert!(ec.hal().locks_taken.get() > 0);
}

#[test]
fn command_roundtrip_all_lengths() {
    for bus in all_bus_kinds() {
        let ec = Ec::probe(SimEc::new(bus, reverse_handler())).unwrap();
        for len in 0..=ec.max_outsize() {
            #[allow(clippy::cast_possible_truncation)]
            let req: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let mut buf = vec![0u8; 0x100];
            buf[..len].copy_from_slice(&req);
            let n = ec.command(0x1234, 1, &mut buf, len, len).unwrap();
            assert_eq!(n, len);
            let expect: Vec<u8> = req.iter().rev().copied().collect();
            assert_eq!(&buf[..n], &expect[..], "bus mismatch at len {len}");
        }
    }
}

#[test]
fn typed_get_version_and_features() {
    for bus in all_bus_kinds() {
        let ec = Ec::probe(SimEc::new(bus, version_handler())).unwrap();
        let mut v = EcResponseGetVersion::zeroed();
        ec.command_typed(commands::EC_CMD_GET_VERSION, 0, &[0u8; 0], &mut v)
            .unwrap();
        assert_eq!(&v.version_string_ro[..9], b"ro-1.2.3\0");
        assert_eq!(&v.version_string_rw[..9], b"rw-4.5.6\0");

        let mut f = EcResponseGetFeatures::zeroed();
        ec.command_typed(commands::EC_CMD_GET_FEATURES, 0, &[0u8; 0], &mut f)
            .unwrap();
        let flags = f.flags;
        assert_eq!(flags, [0x8000_0001, 2]);
    }
}

#[test]
fn ec_result_is_reported() {
    for bus in all_bus_kinds() {
        let ec = Ec::probe(SimEc::new(bus, version_handler())).unwrap();
        let mut buf = [0u8; 16];
        let err = ec.command(0x99, 0, &mut buf, 0, 16).unwrap_err();
        assert_eq!(err, CommandError::EcResult(res::INVALID_COMMAND));
        assert_eq!(err.legacy_code(), -1001);
    }
}

#[test]
fn response_larger_than_insize() {
    for bus in all_bus_kinds() {
        let ec = Ec::probe(SimEc::new(bus, reverse_handler())).unwrap();
        let mut buf = [0u8; 16];
        let err = ec.command(1, 0, &mut buf, 8, 4).unwrap_err();
        let expect = if ec.protocol() == Protocol::V3 {
            res::RESPONSE_TOO_BIG
        } else {
            res::INVALID_RESPONSE
        };
        assert_eq!(err, CommandError::Transport(expect));
    }
}

#[test]
fn bad_response_checksum() {
    for bus in all_bus_kinds() {
        let ec = Ec::probe(SimEc::new(bus, reverse_handler())).unwrap();
        ec.hal().corrupt_response.set(true);
        let mut buf = [1u8; 16];
        let err = ec.command(1, 0, &mut buf, 5, 5).unwrap_err();
        assert_eq!(err, CommandError::Transport(res::INVALID_CHECKSUM));
        assert_eq!(err.legacy_code(), -7);
    }
}

#[test]
fn timeout() {
    let mut sim = SimEc::new(all_bus_kinds()[1], reverse_handler());
    sim.busy_forever = true;
    let ec = Ec::probe(sim).unwrap();
    let mut buf = [0u8; 4];
    let err = ec.command(1, 0, &mut buf, 0, 4).unwrap_err();
    assert_eq!(err, CommandError::Transport(res::ERROR));
    assert!(ec.hal().now.get() >= COMMAND_TIMEOUT_US);
}

#[test]
fn oversized_requests_rejected() {
    for bus in all_bus_kinds() {
        let ec = Ec::probe(SimEc::new(bus, reverse_handler())).unwrap();
        let mut buf = vec![0u8; 0x200];
        let out = ec.max_outsize() + 1;
        let err = ec.command(1, 0, &mut buf, out, 0).unwrap_err();
        assert_eq!(err, CommandError::Transport(res::REQUEST_TRUNCATED));
        // Buffer shorter than requested sizes.
        let err = ec.command(1, 0, &mut buf[..4], 8, 0).unwrap_err();
        assert_eq!(err, CommandError::Transport(res::INVALID_PARAM));
    }
}

#[test]
fn read_mem_fixed_and_string() {
    for (bus, str_off) in [(all_bus_kinds()[1], 0x40), (Bus::Mec, 0x41)] {
        let ec = Ec::probe(SimEc::new(bus, reverse_handler())).unwrap();
        let mut dest = [0u8; 255];
        assert_eq!(ec.read_mem(0x20, 2, &mut dest), Ok(2));
        assert_eq!(&dest[..2], b"EC");

        dest.fill(0xAA);
        assert_eq!(ec.read_mem(str_off, 0, &mut dest), Ok(6));
        assert_eq!(&dest[..6], b"hello\0");
        assert_eq!(dest[6], 0xAA, "string read must stop at NUL");

        // Same bounds as the C driver: offset >= EC_MEMMAP_SIZE - bytes.
        assert_eq!(ec.read_mem(0xFE, 1, &mut dest), Err(()));
        assert_eq!(ec.read_mem(0xF0, 15, &mut dest), Err(()));
        assert_eq!(ec.read_mem(0xF0, 14, &mut dest), Ok(14));
        assert_eq!(ec.read_mem(0, 256, &mut dest), Err(()));
        // Destination too small.
        assert_eq!(ec.read_mem(0, 4, &mut dest[..3]), Err(()));
    }
}

#[test]
fn mec_unaligned_short_access_stays_in_bounds() {
    let ec = Ec::probe(SimEc::new(Bus::Mec, reverse_handler())).unwrap();
    // 0xE21 is 1 mod 4; the C driver would have written 3 bytes here.
    let mut one = [0u8; 1];
    ec.read_bytes(0xE21, &mut one);
    assert_eq!(one, [b'C']);

    // Unaligned write then read back across all alignments and lengths.
    for start in 0x900u16..0x904 {
        for len in 0..12usize {
            #[allow(clippy::cast_possible_truncation)]
            let data: Vec<u8> = (0..len).map(|i| (i as u8) ^ 0x5A ^ (start as u8)).collect();
            ec.hal().ram.borrow_mut()[0x0F0..0x120].fill(0xEE);
            ec.write_bytes(start, &data);
            let ram = ec.hal().ram.borrow();
            let base = usize::from(start - 0x800);
            assert_eq!(&ram[base..base + len], &data[..]);
            assert_eq!(
                ram[base + len],
                0xEE,
                "overwrite past end at {start:#x}/{len}"
            );
            assert_eq!(
                ram[base - 1],
                0xEE,
                "overwrite before start at {start:#x}/{len}"
            );
            drop(ram);
            let mut back = vec![0u8; len];
            ec.read_bytes(start, &mut back);
            assert_eq!(back, data);
        }
    }
}

#[test]
fn mec_range_classification() {
    assert_eq!(mec_in_range(0x200, 1), MecRange::Outside);
    assert_eq!(mec_in_range(0x800, 8), MecRange::Inside);
    assert_eq!(mec_in_range(0xE00, 255), MecRange::Inside);
    assert_eq!(mec_in_range(0xE00, 256), MecRange::Invalid);
    assert_eq!(mec_in_range(0x7FF, 2), MecRange::Invalid);
    assert_eq!(mec_in_range(0x800, 0), MecRange::Invalid);
    assert_eq!(mec_in_range(0xF00, 1), MecRange::Outside);
}

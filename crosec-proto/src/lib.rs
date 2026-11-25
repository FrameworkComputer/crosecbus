//! Chrome EC host command transport over LPC (protocol v2 and v3) and
//! Microchip MEC LPC (EMI window), ported from `comm-lpc.c` and
//! `comm-mec_lpc.c` of the C `crosecbus` driver.
//!
//! This crate has no dependency on the WDK: the driver supplies port I/O,
//! timing and locking through [`Hal`]. That keeps the protocol logic testable
//! on the host against a simulated EC.

#![cfg_attr(not(test), no_std)]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]

pub mod commands;

use core::fmt;

use commands::res;

pub const EC_LPC_ADDR_HOST_DATA: u16 = 0x200;
pub const EC_LPC_ADDR_HOST_CMD: u16 = 0x204;
pub const EC_LPC_ADDR_HOST_ARGS: u16 = 0x800;
pub const EC_LPC_ADDR_HOST_PARAM: u16 = 0x804;
pub const EC_LPC_ADDR_HOST_PACKET: u16 = 0x800;
pub const EC_LPC_HOST_PACKET_SIZE: usize = 0x100;
pub const EC_HOST_CMD_REGION0: u16 = 0x800;
pub const EC_LPC_ADDR_MEMMAP: u16 = 0xE00;
pub const EC_MEMMAP_SIZE: usize = 255;
pub const EC_MEMMAP_ID: u16 = 0x20;
pub const EC_MEMMAP_HOST_CMD_FLAGS: u16 = 0x27;
pub const EC_HOST_CMD_FLAG_LPC_ARGS_SUPPORTED: u8 = 0x01;
pub const EC_HOST_CMD_FLAG_VERSION_3: u8 = 0x02;
pub const EC_PROTO2_MAX_PARAM_SIZE: usize = 0xFC;
pub const EC_COMMAND_PROTOCOL_3: u8 = 0xDA;
pub const EC_HOST_REQUEST_VERSION: u8 = 3;
pub const EC_HOST_RESPONSE_VERSION: u8 = 3;
pub const EC_HOST_ARGS_FLAG_FROM_HOST: u8 = 0x01;
pub const EC_HOST_ARGS_FLAG_TO_HOST: u8 = 0x02;
pub const EC_LPC_STATUS_FROM_HOST: u8 = 0x02;
pub const EC_LPC_STATUS_PROCESSING: u8 = 0x04;
pub const EC_LPC_STATUS_BUSY_MASK: u8 = EC_LPC_STATUS_FROM_HOST | EC_LPC_STATUS_PROCESSING;

/// `sizeof(struct ec_host_request)` / `sizeof(struct ec_host_response)`
const HOST_PACKET_HEADER_LEN: usize = 8;
/// `sizeof(struct ec_lpc_host_args)`
const LPC_HOST_ARGS_LEN: usize = 4;

const COMMAND_TIMEOUT_US: u64 = 1_000_000;

/// MEC EMI registers, relative to [`EC_HOST_CMD_REGION0`].
const MEC_EMI_EC_ADDRESS_B0: u16 = EC_HOST_CMD_REGION0 + 2;
const MEC_EMI_EC_DATA_B0: u16 = EC_HOST_CMD_REGION0 + 4;
const MEC_EMI_EC_DATA_B2: u16 = EC_HOST_CMD_REGION0 + 6;
const MEC_EMI_BASE: u16 = EC_HOST_CMD_REGION0;
#[allow(clippy::cast_possible_truncation)]
const MEC_EMI_END: u16 = EC_LPC_ADDR_MEMMAP + EC_MEMMAP_SIZE as u16;

#[derive(Clone, Copy)]
#[repr(u16)]
enum MecAccess {
    Byte = 0x0,
    LongAutoIncrement = 0x3,
}

/// Platform services needed by the transport.
pub trait Hal {
    /// Held while the MEC EMI window is in use.
    type Guard<'a>
    where
        Self: 'a;

    fn inb(&self, port: u16) -> u8;
    fn outb(&self, port: u16, value: u8);
    fn inw(&self, port: u16) -> u16;
    fn outw(&self, port: u16, value: u16);
    /// Sleep for at least `us` microseconds.
    fn delay_us(&self, us: u32);
    /// Monotonic-enough clock in microseconds.
    fn now_us(&self) -> u64;
    /// Serialises access to the MEC EMI window, which needs an address write
    /// followed by data accesses.
    fn lock_mec(&self) -> Self::Guard<'_>;
    fn log(&self, _args: fmt::Arguments<'_>) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    V2,
    V3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeError {
    /// Host command ports read back as all ones.
    NotPresent,
    /// No `EC` signature in the memory map.
    NoMemoryMap,
    /// EC supports neither protocol v2 nor v3.
    UnsupportedProtocol,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// The EC completed the command with a non-zero `ec_status`.
    EcResult(u8),
    /// The transfer itself failed; the value is the `ec_status` that best
    /// describes the failure.
    Transport(u8),
}

impl CommandError {
    /// The value the C `ec_command_proto` returned: `-EECRESULT - result` for
    /// EC results, `-ec_status` for transport failures.
    #[must_use]
    pub fn legacy_code(self) -> i32 {
        const EECRESULT: i32 = 1000;
        match self {
            Self::EcResult(r) => -EECRESULT - i32::from(r),
            Self::Transport(r) => -i32::from(r),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MecRange {
    Outside,
    Inside,
    /// Straddles the window, or is zero-length. Like the C driver, this is
    /// still sent through the EMI window.
    Invalid,
}

fn mec_in_range(offset: u16, length: usize) -> MecRange {
    let offset = usize::from(offset);
    let (base, end) = (usize::from(MEC_EMI_BASE), usize::from(MEC_EMI_END));
    if length == 0 {
        return MecRange::Invalid;
    }
    if offset >= base && offset < end {
        if offset + length > end {
            return MecRange::Invalid;
        }
        return MecRange::Inside;
    }
    if offset + length > base && offset < end {
        return MecRange::Invalid;
    }
    MecRange::Outside
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b))
}

/// A probed Chrome EC.
pub struct Ec<H: Hal> {
    hal: H,
    mec: bool,
    protocol: Protocol,
    max_outsize: usize,
    max_insize: usize,
}

impl<H: Hal> Ec<H> {
    /// Detects the EC and the protocol it speaks (`comm_init_lpc`).
    pub fn probe(hal: H) -> Result<Self, ProbeError> {
        // Chromium EC guarantees that at least one status bit will be 0, so
        // if the command and data bytes are both 0xff, very likely that
        // Chromium EC is not present. See crosbug.com/p/10963.
        if hal.inb(EC_LPC_ADDR_HOST_CMD) & hal.inb(EC_LPC_ADDR_HOST_DATA) == 0xFF {
            hal.log(format_args!(
                "Port {EC_LPC_ADDR_HOST_CMD:#x},{EC_LPC_ADDR_HOST_DATA:#x} are both 0xFF. Very \
                 likely this board doesn't have a Chromium EC."
            ));
            return Err(ProbeError::NotPresent);
        }

        let packet_max = EC_LPC_HOST_PACKET_SIZE - HOST_PACKET_HEADER_LEN;
        let mut ec = Self {
            hal,
            mec: true,
            protocol: Protocol::V3,
            max_outsize: packet_max,
            max_insize: packet_max,
        };

        // Check for a MEC first. All MEC ECs are protocol v3.
        let mut signature = [0u8; 2];
        ec.read_bytes(EC_LPC_ADDR_MEMMAP + EC_MEMMAP_ID, &mut signature);
        if &signature == b"EC" {
            ec.hal.log(format_args!("MEC EC"));
            return Ok(ec);
        }

        // The cheapest way to check for LPC args support is the memory-mapped
        // flag, rather than sending a 'hello' command.
        ec.mec = false;
        ec.read_bytes(EC_LPC_ADDR_MEMMAP + EC_MEMMAP_ID, &mut signature);
        ec.hal.log(format_args!(
            "Signature: {:#x} {:#x}",
            signature[0], signature[1]
        ));
        if &signature != b"EC" {
            ec.hal.log(format_args!("Missing Chromium EC memory map."));
            return Err(ProbeError::NoMemoryMap);
        }

        let flags = ec.hal.inb(EC_LPC_ADDR_MEMMAP + EC_MEMMAP_HOST_CMD_FLAGS);
        if flags & EC_HOST_CMD_FLAG_VERSION_3 != 0 {
            ec.hal.log(format_args!("Ver 3"));
        } else if flags & EC_HOST_CMD_FLAG_LPC_ARGS_SUPPORTED != 0 {
            ec.protocol = Protocol::V2;
            ec.max_outsize = EC_PROTO2_MAX_PARAM_SIZE;
            ec.max_insize = EC_PROTO2_MAX_PARAM_SIZE;
            ec.hal.log(format_args!("Ver 2"));
        } else {
            ec.hal
                .log(format_args!("EC doesn't support protocols we need."));
            return Err(ProbeError::UnsupportedProtocol);
        }

        ec.hal.log(format_args!(
            "ec_max_insize={} ec_max_outsize={}",
            ec.max_insize, ec.max_outsize
        ));
        Ok(ec)
    }

    pub const fn hal(&self) -> &H {
        &self.hal
    }

    pub const fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub const fn is_mec(&self) -> bool {
        self.mec
    }

    pub const fn max_outsize(&self) -> usize {
        self.max_outsize
    }

    pub const fn max_insize(&self) -> usize {
        self.max_insize
    }

    /// Sends a host command.
    ///
    /// The request is the first `outsize` bytes of `buf`; the response
    /// overwrites the start of `buf`, up to `insize` bytes. Returns the
    /// number of response bytes.
    pub fn command(
        &self,
        command: u16,
        version: u8,
        buf: &mut [u8],
        outsize: usize,
        insize: usize,
    ) -> Result<usize, CommandError> {
        if buf.len() < outsize.max(insize) {
            return Err(CommandError::Transport(res::INVALID_PARAM));
        }
        match self.protocol {
            Protocol::V2 => self.command_v2(command, version, buf, outsize, insize),
            Protocol::V3 => self.command_v3(command, version, buf, outsize, insize),
        }
    }

    /// Typed convenience wrapper over [`Self::command`].
    pub fn command_typed<P: commands::Pod, R: commands::Pod>(
        &self,
        command: u16,
        version: u8,
        params: &P,
        response: &mut R,
    ) -> Result<usize, CommandError> {
        let mut buf = [0u8; EC_LPC_HOST_PACKET_SIZE];
        let (out, inn) = (core::mem::size_of::<P>(), core::mem::size_of::<R>());
        if out.max(inn) > buf.len() {
            return Err(CommandError::Transport(res::REQUEST_TRUNCATED));
        }
        buf[..out].copy_from_slice(params.as_bytes());
        let n = self.command(command, version, &mut buf, out, inn)?;
        response.as_bytes_mut()[..n].copy_from_slice(&buf[..n]);
        Ok(n)
    }

    /// Reads the EC memory map (`ec_readmem`). `bytes == 0` reads a
    /// NUL-terminated string, including the terminator. Returns the number of
    /// bytes read.
    #[allow(clippy::result_unit_err, clippy::cast_possible_truncation)]
    pub fn read_mem(&self, offset: usize, bytes: usize, dest: &mut [u8]) -> Result<usize, ()> {
        if bytes > EC_MEMMAP_SIZE || offset >= EC_MEMMAP_SIZE - bytes {
            return Err(());
        }
        // offset < EC_MEMMAP_SIZE, so the port numbers below fit in u16.
        if bytes > 0 {
            let dest = dest.get_mut(..bytes).ok_or(())?;
            self.read_bytes(EC_LPC_ADDR_MEMMAP + offset as u16, dest);
            return Ok(bytes);
        }

        let mut count = 0;
        for (i, slot) in (offset..EC_MEMMAP_SIZE).zip(dest.iter_mut()) {
            self.read_bytes(EC_LPC_ADDR_MEMMAP + i as u16, core::slice::from_mut(slot));
            count += 1;
            if *slot == 0 {
                break;
            }
        }
        Ok(count)
    }

    /// Waits for the EC to drop its busy flag. Returns `false` on timeout.
    fn wait_for_ec(&self, status_port: u16, timeout_us: u64) -> bool {
        let start = self.hal.now_us();

        // Delay first, in case we just sent out a command but the EC hasn't
        // raised the busy flag.
        self.hal.delay_us(200);

        loop {
            if self.hal.now_us() > start + timeout_us {
                return false;
            }
            if self.hal.inb(status_port) & EC_LPC_STATUS_BUSY_MASK == 0 {
                return true;
            }
            self.hal.delay_us(100);
        }
    }

    fn command_v2(
        &self,
        command: u16,
        version: u8,
        buf: &mut [u8],
        outsize: usize,
        insize: usize,
    ) -> Result<usize, CommandError> {
        if outsize > EC_PROTO2_MAX_PARAM_SIZE {
            return Err(CommandError::Transport(res::REQUEST_TRUNCATED));
        }
        #[allow(clippy::cast_possible_truncation)]
        let (command_lo, data_size) = (command as u8, outsize as u8);

        let mut args: [u8; LPC_HOST_ARGS_LEN] =
            [EC_HOST_ARGS_FLAG_FROM_HOST, version, data_size, 0];
        let mut csum = command_lo
            .wrapping_add(args[0])
            .wrapping_add(args[1])
            .wrapping_add(args[2]);

        for (port, b) in (EC_LPC_ADDR_HOST_PARAM..).zip(&buf[..outsize]) {
            self.hal.outb(port, *b);
            csum = csum.wrapping_add(*b);
        }

        args[3] = csum;
        for (port, b) in (EC_LPC_ADDR_HOST_ARGS..).zip(args) {
            self.hal.outb(port, b);
        }

        self.hal.outb(EC_LPC_ADDR_HOST_CMD, command_lo);

        if !self.wait_for_ec(EC_LPC_ADDR_HOST_CMD, COMMAND_TIMEOUT_US) {
            self.hal
                .log(format_args!("Timeout waiting for EC response"));
            return Err(CommandError::Transport(res::ERROR));
        }

        let result = self.hal.inb(EC_LPC_ADDR_HOST_DATA);
        if result != 0 {
            self.log_ec_result(result, command, version);
            return Err(CommandError::EcResult(result));
        }

        for (port, b) in (EC_LPC_ADDR_HOST_ARGS..).zip(args.iter_mut()) {
            *b = self.hal.inb(port);
        }
        let [flags, rversion, rsize, rcsum] = args;

        // If the EC didn't modify the args flags, then somehow we sent a
        // new-style command to an old EC, which means it would have read its
        // params from the wrong place.
        if flags & EC_HOST_ARGS_FLAG_TO_HOST == 0 {
            self.hal.log(format_args!("EC protocol mismatch"));
            return Err(CommandError::Transport(res::INVALID_RESPONSE));
        }

        let rsize_usize = usize::from(rsize);
        if rsize_usize > insize {
            self.hal.log(format_args!("EC returned too much data"));
            return Err(CommandError::Transport(res::INVALID_RESPONSE));
        }

        let mut csum = command_lo
            .wrapping_add(flags)
            .wrapping_add(rversion)
            .wrapping_add(rsize);
        for (port, b) in (EC_LPC_ADDR_HOST_PARAM..).zip(&mut buf[..rsize_usize]) {
            *b = self.hal.inb(port);
            csum = csum.wrapping_add(*b);
        }

        if rcsum != csum {
            self.hal
                .log(format_args!("EC response has invalid checksum"));
            return Err(CommandError::Transport(res::INVALID_CHECKSUM));
        }

        Ok(rsize_usize)
    }

    fn command_v3(
        &self,
        command: u16,
        version: u8,
        buf: &mut [u8],
        outsize: usize,
        insize: usize,
    ) -> Result<usize, CommandError> {
        if outsize + HOST_PACKET_HEADER_LEN > EC_LPC_HOST_PACKET_SIZE {
            return Err(CommandError::Transport(res::REQUEST_TRUNCATED));
        }

        #[allow(clippy::cast_possible_truncation)]
        let data_len = (outsize as u16).to_le_bytes();
        let command_le = command.to_le_bytes();
        let mut rq = [
            EC_HOST_REQUEST_VERSION,
            0, // checksum
            command_le[0],
            command_le[1],
            version,
            0, // reserved
            data_len[0],
            data_len[1],
        ];

        let request = &buf[..outsize];
        #[allow(clippy::cast_possible_truncation)]
        self.write_bytes(
            EC_LPC_ADDR_HOST_PACKET + HOST_PACKET_HEADER_LEN as u16,
            request,
        );

        // Write checksum field so the entire packet sums to 0.
        rq[1] = checksum(request).wrapping_add(checksum(&rq)).wrapping_neg();
        self.write_bytes(EC_LPC_ADDR_HOST_PACKET, &rq);

        self.hal.outb(EC_LPC_ADDR_HOST_CMD, EC_COMMAND_PROTOCOL_3);

        if !self.wait_for_ec(EC_LPC_ADDR_HOST_CMD, COMMAND_TIMEOUT_US) {
            self.hal
                .log(format_args!("Timeout waiting for EC response"));
            return Err(CommandError::Transport(res::ERROR));
        }

        let result = self.hal.inb(EC_LPC_ADDR_HOST_DATA);
        if result != 0 {
            self.log_ec_result(result, command, version);
            return Err(CommandError::EcResult(result));
        }

        let mut rs = [0u8; HOST_PACKET_HEADER_LEN];
        self.read_bytes(EC_LPC_ADDR_HOST_PACKET, &mut rs);
        let mut csum = checksum(&rs);

        if rs[0] != EC_HOST_RESPONSE_VERSION {
            self.hal.log(format_args!("EC response version mismatch"));
            return Err(CommandError::Transport(res::INVALID_RESPONSE));
        }

        if rs[6] != 0 || rs[7] != 0 {
            self.hal.log(format_args!("EC response reserved != 0"));
            return Err(CommandError::Transport(res::INVALID_RESPONSE));
        }

        let data_len = usize::from(u16::from_le_bytes([rs[4], rs[5]]));
        if data_len > insize {
            self.hal.log(format_args!("EC returned too much data"));
            return Err(CommandError::Transport(res::RESPONSE_TOO_BIG));
        }

        let response = &mut buf[..data_len];
        #[allow(clippy::cast_possible_truncation)]
        self.read_bytes(
            EC_LPC_ADDR_HOST_PACKET + HOST_PACKET_HEADER_LEN as u16,
            response,
        );
        csum = csum.wrapping_add(checksum(response));

        if csum != 0 {
            self.hal
                .log(format_args!("EC response has invalid checksum"));
            return Err(CommandError::Transport(res::INVALID_CHECKSUM));
        }

        Ok(data_len)
    }

    fn log_ec_result(&self, result: u8, command: u16, version: u8) {
        self.hal.log(format_args!(
            "EC returned error result code {result} ({}), cmd={command},ver={version}",
            res::to_str(result)
        ));
    }

    /// `ec_lpc_ops.read`
    #[allow(clippy::cast_possible_truncation)]
    fn read_bytes(&self, port: u16, dest: &mut [u8]) {
        if self.mec && mec_in_range(port, dest.len()) != MecRange::Outside {
            self.mec_read(port - EC_HOST_CMD_REGION0, dest);
            return;
        }
        for (i, b) in dest.iter_mut().enumerate() {
            *b = self.hal.inb(port + i as u16);
        }
    }

    /// `ec_lpc_ops.write`
    #[allow(clippy::cast_possible_truncation)]
    fn write_bytes(&self, port: u16, src: &[u8]) {
        if self.mec && mec_in_range(port, src.len()) != MecRange::Outside {
            self.mec_write(port - EC_HOST_CMD_REGION0, src);
            return;
        }
        for (i, b) in src.iter().enumerate() {
            self.hal.outb(port + i as u16, *b);
        }
    }

    fn mec_set_address(&self, address: u16, access: MecAccess) {
        self.hal
            .outw(MEC_EMI_EC_ADDRESS_B0, (address & 0xFFFC) | access as u16);
    }

    /// `ec_mec_xfer(EC_MEC_READ, ...)`
    fn mec_read(&self, mut address: u16, data: &mut [u8]) {
        let _guard = self.hal.lock_mec();
        let mut pos = 0;

        // Unaligned start address. Unlike the C driver, stop at the end of
        // `data` rather than always finishing the dword.
        if !address.is_multiple_of(4) {
            self.mec_set_address(address, MecAccess::Byte);
            for i in (address % 4)..4 {
                let Some(slot) = data.get_mut(pos) else { break };
                *slot = self.hal.inb(MEC_EMI_EC_DATA_B0 + i);
                pos += 1;
            }
            address = (address + 4) & 0xFFFC;
        }

        if data.len() - pos >= 4 {
            self.mec_set_address(address, MecAccess::LongAutoIncrement);
            for chunk in data[pos..].as_chunks_mut::<4>().0 {
                let lo = self.hal.inw(MEC_EMI_EC_DATA_B0).to_le_bytes();
                let hi = self.hal.inw(MEC_EMI_EC_DATA_B2).to_le_bytes();
                chunk.copy_from_slice(&[lo[0], lo[1], hi[0], hi[1]]);
                pos += 4;
                address += 4;
            }
        }

        if pos < data.len() {
            self.mec_set_address(address, MecAccess::Byte);
            for (port, slot) in (MEC_EMI_EC_DATA_B0..).zip(&mut data[pos..]) {
                *slot = self.hal.inb(port);
            }
        }
    }

    /// `ec_mec_xfer(EC_MEC_WRITE, ...)`
    fn mec_write(&self, mut address: u16, data: &[u8]) {
        let _guard = self.hal.lock_mec();
        let mut pos = 0;

        if !address.is_multiple_of(4) {
            self.mec_set_address(address, MecAccess::Byte);
            for i in (address % 4)..4 {
                let Some(b) = data.get(pos) else { break };
                self.hal.outb(MEC_EMI_EC_DATA_B0 + i, *b);
                pos += 1;
            }
            address = (address + 4) & 0xFFFC;
        }

        if data.len() - pos >= 4 {
            self.mec_set_address(address, MecAccess::LongAutoIncrement);
            for chunk in data[pos..].as_chunks::<4>().0 {
                self.hal
                    .outw(MEC_EMI_EC_DATA_B0, u16::from_le_bytes([chunk[0], chunk[1]]));
                self.hal
                    .outw(MEC_EMI_EC_DATA_B2, u16::from_le_bytes([chunk[2], chunk[3]]));
                pos += 4;
                address += 4;
            }
        }

        if pos < data.len() {
            self.mec_set_address(address, MecAccess::Byte);
            for (port, b) in (MEC_EMI_EC_DATA_B0..).zip(&data[pos..]) {
                self.hal.outb(port, *b);
            }
        }
    }
}

#[cfg(test)]
mod tests;

//! # Abstract
//!
//!    Chrome EC bus driver. Talks to a Chrome EC over LPC (protocol v2 / v3)
//!    or Microchip MEC LPC, and exposes it to
//!
//!    * other kernel drivers, through the `GUID_CROSEC_INTERFACE_STANDARD` (v1)
//!      and `GUID_CROSEC_INTERFACE_STANDARD_V2` query interfaces, and
//!    * user mode, through `\Device\CrosEC` with `IOCTL_CROSEC_XCMD` and
//!      `IOCTL_CROSEC_RDMEM` (same ABI as `DHowett`'s `FrameworkWindowsUtils`
//!      driver).
//!
//!    Rust port of <https://github.com/coolstar/crosecbus>.

#![no_std]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![allow(clippy::missing_safety_doc)]

#[cfg(not(target_arch = "x86_64"))]
compile_error!("crosecbus uses x86 port I/O and only supports x86_64");

mod device;
mod driver;
mod hal;
mod queue;
mod wdf_object_context;

#[cfg(not(test))]
extern crate wdk_panic;

use core::sync::atomic::AtomicI64;

use crosec_proto::Ec;
use wdf_object_context::wdf_declare_context_type_with_name;
#[cfg(not(test))]
use wdk_alloc::WdkAllocator;
use wdk_sys::{
    call_unsafe_wdf_function_binding,
    BOOLEAN,
    GUID,
    INTERFACE,
    NTSTATUS,
    PCALLBACK_OBJECT,
    PVOID,
    ULONG,
    WDFINTERRUPT,
    WDFOBJECT,
    WDFWAITLOCK,
    WDF_OBJECT_CONTEXT_TYPE_INFO,
};

use crate::hal::KernelHal;

#[cfg(not(test))]
#[global_allocator]
static GLOBAL_ALLOCATOR: WdkAllocator = WdkAllocator;

// {D7062676-E3A4-11EC-A6C4-244BFE9946D0}
pub const GUID_CROSEC_INTERFACE_STANDARD: GUID = GUID {
    Data1: 0xD706_2676,
    Data2: 0xE3A4,
    Data3: 0x11EC,
    Data4: [0xA6, 0xC4, 0x24, 0x4B, 0xFE, 0x99, 0x46, 0xD0],
};

// {AD8649FA-7C71-11ED-B63C-00155DA449AD}
pub const GUID_CROSEC_INTERFACE_STANDARD_V2: GUID = GUID {
    Data1: 0xAD86_49FA,
    Data2: 0x7C71,
    Data3: 0x11ED,
    Data4: [0xB6, 0x3C, 0x00, 0x15, 0x5D, 0xA4, 0x49, 0xAD],
};

/// `CROSEC_COMMAND`: header of a host command, followed by
/// `max(OutSize, InSize)` bytes of data. Shared by the kernel interface and
/// `IOCTL_CROSEC_XCMD`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CrosEcCommand {
    pub version: u32,
    pub command: u32,
    pub out_size: u32,
    pub in_size: u32,
    pub result: u32,
}

pub const CROSEC_COMMAND_HEADER_LEN: usize = core::mem::size_of::<CrosEcCommand>();
const _: () = assert!(CROSEC_COMMAND_HEADER_LEN == 20);

pub type PfnCrosEcCmdXferStatus =
    unsafe extern "C" fn(context: PVOID, msg: *mut CrosEcCommand) -> NTSTATUS;
pub type PfnCrosEcCheckFeatures = unsafe extern "C" fn(context: PVOID, feature: i32) -> BOOLEAN;
pub type PfnCrosEcReadMem =
    unsafe extern "C" fn(context: PVOID, offset: i32, bytes: i32, dest: PVOID) -> i32;

/// `CROSEC_INTERFACE_STANDARD`
#[repr(C)]
pub struct CrosEcInterfaceStandard {
    pub interface_header: INTERFACE,
    pub cmd_xfer_status: Option<PfnCrosEcCmdXferStatus>,
    pub check_features: Option<PfnCrosEcCheckFeatures>,
}

/// `CROSEC_INTERFACE_STANDARD_V2`
#[repr(C)]
pub struct CrosEcInterfaceStandardV2 {
    pub interface_header: INTERFACE,
    pub cmd_xfer_status: Option<PfnCrosEcCmdXferStatus>,
    pub check_features: Option<PfnCrosEcCheckFeatures>,
    pub read_ec_mem: Option<PfnCrosEcReadMem>,
}

/// `CSVivaldiSettingsArg`, sent through
/// `\Callback\CsKeyboardSettingsCallbackAPI`.
#[repr(C, packed)]
pub struct CsVivaldiSettingsArg {
    pub arg_sz: u32,
    pub settings_request: u32,
    pub button: u8,
}

pub const CS_VIVALDI_REQUEST_UPDATE_BUTTON: u32 = 0x101;

pub struct DeviceContext {
    /// `EC_CMD_GET_FEATURES` flags, all ones if the query failed.
    ec_features: [u32; 2],

    /// Kernel clients currently waiting for or holding `ec_lock`. User mode
    /// requests back off while this is non-zero.
    kernel_accesses_waiting: AtomicI64,
    /// Serialises host commands.
    ec_lock: WDFWAITLOCK,
    /// Serialises use of the MEC EMI window (also used by memmap reads,
    /// which don't take `ec_lock`).
    mec_lock: WDFWAITLOCK,

    interrupt: WDFINTERRUPT,
    cs_buttons_callback: PCALLBACK_OBJECT,

    /// Set in `EvtDevicePrepareHardware`.
    ec: Option<Ec<KernelHal>>,
}
wdf_declare_context_type_with_name!(DeviceContext, device_get_context);

/// Size of a WDF structure as a `ULONG`, for their `Size` fields.
macro_rules! wdf_struct_size {
    ($t:ty) => {{
        const S: usize = core::mem::size_of::<$t>();
        const {
            assert!(
                S <= wdk_sys::ULONG::MAX as usize,
                "structure size should fit in ULONG"
            );
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "checked by the const assert above"
        )]
        {
            S as wdk_sys::ULONG
        }
    }};
}
pub(crate) use wdf_struct_size;

const WDF_OBJECT_CONTEXT_TYPE_INFO_SIZE: ULONG = wdf_struct_size!(WDF_OBJECT_CONTEXT_TYPE_INFO);

/// Converts an ASCII string literal to UTF-16 at compile time.
///
/// # Panics
///
/// If `s` is not ASCII or is not `N` bytes long (at compile time when used
/// in a const context, as [`unicode_string!`] does).
#[must_use]
pub const fn ascii_to_utf16<const N: usize>(s: &str) -> [u16; N] {
    let bytes = s.as_bytes();
    assert!(bytes.len() == N, "length mismatch");
    let mut out = [0u16; N];
    let mut i = 0;
    while i < N {
        assert!(bytes[i].is_ascii(), "only ASCII is supported");
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

/// Builds a `UNICODE_STRING` pointing at a static UTF-16 copy of an ASCII
/// literal.
macro_rules! unicode_string {
    ($s:literal) => {{
        const S: &str = $s;
        const N: usize = S.len();
        static W: [u16; N] = $crate::ascii_to_utf16::<N>(S);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "literals are far shorter than u16::MAX / 2"
        )]
        wdk_sys::UNICODE_STRING {
            Length: (N * 2) as u16,
            MaximumLength: (N * 2) as u16,
            Buffer: W.as_ptr().cast_mut(),
        }
    }};
}
pub(crate) use unicode_string;

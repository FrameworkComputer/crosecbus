//! The subset of `ec_commands.h` that the bus driver itself uses.
//!
//! Structure sizes match the C header (`#pragma pack(1)`), since the size of
//! each request/response is what gets sent to the EC as `outsize`/`insize`.

/// EC result codes (`enum ec_status`).
pub mod res {
    pub const SUCCESS: u8 = 0;
    pub const INVALID_COMMAND: u8 = 1;
    pub const ERROR: u8 = 2;
    pub const INVALID_PARAM: u8 = 3;
    pub const ACCESS_DENIED: u8 = 4;
    pub const INVALID_RESPONSE: u8 = 5;
    pub const INVALID_VERSION: u8 = 6;
    pub const INVALID_CHECKSUM: u8 = 7;
    pub const IN_PROGRESS: u8 = 8;
    pub const UNAVAILABLE: u8 = 9;
    pub const TIMEOUT: u8 = 10;
    pub const OVERFLOW: u8 = 11;
    pub const INVALID_HEADER: u8 = 12;
    pub const REQUEST_TRUNCATED: u8 = 13;
    pub const RESPONSE_TOO_BIG: u8 = 14;
    pub const BUS_ERROR: u8 = 15;
    pub const BUSY: u8 = 16;
    pub const INVALID_HEADER_VERSION: u8 = 17;
    pub const INVALID_HEADER_CRC: u8 = 18;
    pub const INVALID_DATA_CRC: u8 = 19;
    pub const DUP_UNAVAILABLE: u8 = 20;

    #[must_use]
    pub const fn to_str(res: u8) -> &'static str {
        match res {
            SUCCESS => "SUCCESS",
            INVALID_COMMAND => "INVALID_COMMAND",
            ERROR => "ERROR",
            INVALID_PARAM => "INVALID_PARAM",
            ACCESS_DENIED => "ACCESS_DENIED",
            INVALID_RESPONSE => "INVALID_RESPONSE",
            INVALID_VERSION => "INVALID_VERSION",
            INVALID_CHECKSUM => "INVALID_CHECKSUM",
            IN_PROGRESS => "IN_PROGRESS",
            UNAVAILABLE => "UNAVAILABLE",
            TIMEOUT => "TIMEOUT",
            OVERFLOW => "OVERFLOW",
            INVALID_HEADER => "INVALID_HEADER",
            REQUEST_TRUNCATED => "REQUEST_TRUNCATED",
            RESPONSE_TOO_BIG => "RESPONSE_TOO_BIG",
            BUS_ERROR => "BUS_ERROR",
            BUSY => "BUSY",
            INVALID_HEADER_VERSION => "INVALID_HEADER_VERSION",
            INVALID_HEADER_CRC => "INVALID_HEADER_CRC",
            INVALID_DATA_CRC => "INVALID_DATA_CRC",
            DUP_UNAVAILABLE => "DUP_UNAVAILABLE",
            _ => "Unknown",
        }
    }
}

pub const EC_CMD_GET_VERSION: u16 = 0x0002;
pub const EC_CMD_GET_FEATURES: u16 = 0x000D;
pub const EC_CMD_FLASH_WRITE: u16 = 0x0012;
pub const EC_CMD_FLASH_ERASE: u16 = 0x0013;
pub const EC_CMD_FLASH_PROTECT: u16 = 0x0015;
pub const EC_CMD_MOTION_SENSE_CMD: u16 = 0x002B;
pub const EC_CMD_GET_NEXT_EVENT: u16 = 0x0067;
pub const EC_CMD_HOST_EVENT_GET_B: u16 = 0x0087;
pub const EC_CMD_HOST_EVENT_CLEAR_B: u16 = 0x008F;
pub const EC_CMD_USB_PD_FW_UPDATE: u16 = 0x0110;

pub const EC_HOST_EVENT_MKBP: u32 = 27;
pub const EC_HOST_EVENT_INVALID: u32 = 32;

#[must_use]
pub const fn ec_host_event_mask(event: u32) -> u32 {
    1 << (event - 1)
}

pub const EC_MKBP_EVENT_BUTTON: u8 = 3;

pub const MOTIONSENSE_CMD_FIFO_INT_ENABLE: u8 = 15;

/// `struct ec_response_get_version`
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EcResponseGetVersion {
    pub version_string_ro: [u8; 32],
    pub version_string_rw: [u8; 32],
    pub reserved: [u8; 32],
    pub current_image: u32,
}

/// `struct ec_response_get_features`
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EcResponseGetFeatures {
    pub flags: [u32; 2],
}

/// `struct ec_params_motion_sense`, with only the `fifo_int_enable` arm of
/// the union spelled out. Padded to the size of the full union.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EcParamsMotionSense {
    pub cmd: u8,
    /// `fifo_int_enable.enable`; `0x7F` (`EC_MOTION_SENSE_NO_VALUE`) reads
    /// the current state.
    pub enable: i8,
    pub reserved: [u8; 10],
}

/// `struct ec_response_motion_sense`, size only.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EcResponseMotionSense {
    pub raw: [u8; 19],
}

/// `struct ec_params_host_event_mask` / `struct ec_response_host_event_mask`
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EcHostEventMask {
    pub mask: u32,
}

/// `struct ec_response_get_next_event`
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EcResponseGetNextEvent {
    pub event_type: u8,
    /// `union ec_response_get_next_data`; for `EC_MKBP_EVENT_BUTTON` the
    /// first four bytes are the `buttons` bitmap.
    pub data: [u8; 13],
}

impl EcResponseGetNextEvent {
    #[must_use]
    pub const fn buttons(&self) -> u32 {
        u32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
    }
}

const _: () = {
    assert!(core::mem::size_of::<EcResponseGetVersion>() == 100);
    assert!(core::mem::size_of::<EcResponseGetFeatures>() == 8);
    assert!(core::mem::size_of::<EcParamsMotionSense>() == 12);
    assert!(core::mem::size_of::<EcResponseMotionSense>() == 19);
    assert!(core::mem::size_of::<EcHostEventMask>() == 4);
    assert!(core::mem::size_of::<EcResponseGetNextEvent>() == 14);
};

/// Plain-old-data that can be sent to / received from the EC as raw bytes.
///
/// # Safety
///
/// Implementors must be `repr(C, packed)` with no padding and valid for any
/// bit pattern.
pub unsafe trait Pod: Copy {
    #[must_use]
    fn zeroed() -> Self {
        // SAFETY: Implementors are valid for any bit pattern.
        unsafe { core::mem::zeroed() }
    }

    fn as_bytes(&self) -> &[u8] {
        // SAFETY: Implementors have no padding.
        unsafe {
            core::slice::from_raw_parts(
                core::ptr::from_ref(self).cast(),
                core::mem::size_of::<Self>(),
            )
        }
    }

    fn as_bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: Implementors have no padding and are valid for any bit
        // pattern.
        unsafe {
            core::slice::from_raw_parts_mut(
                core::ptr::from_mut(self).cast(),
                core::mem::size_of::<Self>(),
            )
        }
    }
}

unsafe impl Pod for EcResponseGetVersion {}
unsafe impl Pod for EcResponseGetFeatures {}
unsafe impl Pod for EcParamsMotionSense {}
unsafe impl Pod for EcResponseMotionSense {}
unsafe impl Pod for EcHostEventMask {}
unsafe impl Pod for EcResponseGetNextEvent {}
unsafe impl<const N: usize> Pod for [u8; N] {}

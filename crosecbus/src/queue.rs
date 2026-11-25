//! User mode interface: `IOCTL_CROSEC_XCMD` and `IOCTL_CROSEC_RDMEM` on
//! `\Device\CrosEC`.
//!
//! Mostly based on `DHowett`'s `FrameworkWindowsUtils`, to preserve
//! compatibility with his driver.

use core::sync::atomic::Ordering;

use crosec_proto::{
    commands::{
        res,
        EC_CMD_FLASH_ERASE,
        EC_CMD_FLASH_PROTECT,
        EC_CMD_FLASH_WRITE,
        EC_CMD_USB_PD_FW_UPDATE,
    },
    CommandError,
    EC_LPC_HOST_PACKET_SIZE,
};
use wdk::{paged_code, println};
use wdk_sys::{
    call_unsafe_wdf_function_binding,
    _WDF_IO_QUEUE_DISPATCH_TYPE,
    _WDF_TRI_STATE,
    NTSTATUS,
    PVOID,
    STATUS_ACCESS_DENIED,
    STATUS_BUFFER_OVERFLOW,
    STATUS_BUFFER_TOO_SMALL,
    STATUS_CRC_ERROR,
    STATUS_DATA_ERROR,
    STATUS_DEVICE_BUSY,
    STATUS_DEVICE_NOT_READY,
    STATUS_FAIL_CHECK,
    STATUS_INVALID_ADDRESS,
    STATUS_INVALID_PARAMETER,
    STATUS_IO_TIMEOUT,
    STATUS_RETRY,
    STATUS_SUCCESS,
    STATUS_UNSUCCESSFUL,
    ULONG,
    WDFDEVICE,
    WDFOBJECT,
    WDFQUEUE,
    WDFREQUEST,
    WDF_IO_QUEUE_CONFIG,
    WDF_NO_OBJECT_ATTRIBUTES,
};

use crate::{
    device_get_context,
    hal::{delay_us, WaitLockGuard},
    wdf_struct_size,
    CrosEcCommand,
    DeviceContext,
    CROSEC_COMMAND_HEADER_LEN,
};

const FILE_DEVICE_CROS_EMBEDDED_CONTROLLER: u32 = 0x80EC;
const METHOD_BUFFERED: u32 = 0;
const FILE_READ_DATA: u32 = 0x0001;
const FILE_WRITE_DATA: u32 = 0x0002;

const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

pub const IOCTL_CROSEC_XCMD: u32 = ctl_code(
    FILE_DEVICE_CROS_EMBEDDED_CONTROLLER,
    0x801,
    METHOD_BUFFERED,
    FILE_READ_DATA | FILE_WRITE_DATA,
);
pub const IOCTL_CROSEC_RDMEM: u32 = ctl_code(
    FILE_DEVICE_CROS_EMBEDDED_CONTROLLER,
    0x802,
    METHOD_BUFFERED,
    FILE_READ_DATA,
);

const CROSEC_MEMMAP_SIZE: usize = 0xFF;

/// `CROSEC_STATUS_IN_PROGRESS`: EC command in progress.
#[allow(clippy::cast_possible_wrap, reason = "NTSTATUS customer error code")]
const CROSEC_STATUS_IN_PROGRESS: NTSTATUS = 0xE0EC_0001_u32 as NTSTATUS;
/// `CROSEC_STATUS_UNAVAILABLE`: EC not available.
#[allow(clippy::cast_possible_wrap, reason = "NTSTATUS customer error code")]
const CROSEC_STATUS_UNAVAILABLE: NTSTATUS = 0xE0EC_0002_u32 as NTSTATUS;

/// `CROSEC_READMEM`
#[repr(C)]
struct CrosEcReadMem {
    offset: ULONG,
    bytes: ULONG,
    buffer: [u8; CROSEC_MEMMAP_SIZE],
}
const CROSEC_READMEM_LEN: usize = core::mem::size_of::<CrosEcReadMem>();
const CROSEC_READMEM_BUFFER_OFFSET: usize = core::mem::offset_of!(CrosEcReadMem, buffer);
const _: () = assert!(CROSEC_READMEM_LEN == 264);

/// Maps a transport failure (`-ec_status`) to an `NTSTATUS`
/// (`sCrosECErrorCodeMapping`).
const fn transport_error_status(code: u8) -> NTSTATUS {
    match code {
        res::SUCCESS => STATUS_SUCCESS,
        res::INVALID_COMMAND | res::INVALID_PARAM => STATUS_INVALID_PARAMETER,
        res::ERROR | res::BUS_ERROR | res::DUP_UNAVAILABLE => STATUS_UNSUCCESSFUL,
        res::ACCESS_DENIED => STATUS_ACCESS_DENIED,
        res::INVALID_RESPONSE
        | res::INVALID_VERSION
        | res::INVALID_HEADER
        | res::INVALID_HEADER_VERSION => STATUS_DATA_ERROR,
        res::INVALID_CHECKSUM | res::INVALID_HEADER_CRC | res::INVALID_DATA_CRC => STATUS_CRC_ERROR,
        res::IN_PROGRESS => CROSEC_STATUS_IN_PROGRESS,
        res::UNAVAILABLE => CROSEC_STATUS_UNAVAILABLE,
        res::TIMEOUT => STATUS_IO_TIMEOUT,
        res::OVERFLOW | res::RESPONSE_TOO_BIG => STATUS_BUFFER_OVERFLOW,
        res::REQUEST_TRUNCATED => STATUS_BUFFER_TOO_SMALL,
        res::BUSY => STATUS_DEVICE_BUSY,
        _ => STATUS_FAIL_CHECK,
    }
}

/// Creates the default parallel queue for user mode IOCTLs.
#[link_section = "PAGE"]
pub unsafe fn crosecbus_queue_initialize(device: WDFDEVICE) -> NTSTATUS {
    paged_code!();

    let mut queue_config = WDF_IO_QUEUE_CONFIG {
        Size: wdf_struct_size!(WDF_IO_QUEUE_CONFIG),
        PowerManaged: _WDF_TRI_STATE::WdfUseDefault,
        DefaultQueue: u8::from(true),
        DispatchType: _WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel,
        EvtIoDeviceControl: Some(crosec_evt_io_device_control),
        ..WDF_IO_QUEUE_CONFIG::default()
    };
    // As WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE does for parallel queues: no
    // limit on requests presented at once. Left at 0, WDF accepts requests
    // but never presents any to the driver, so every IOCTL hangs.
    queue_config.Settings.Parallel.NumberOfPresentedRequests = ULONG::MAX;

    let mut queue: WDFQUEUE = core::ptr::null_mut();
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &raw mut queue_config,
            WDF_NO_OBJECT_ATTRIBUTES,
            &raw mut queue,
        )
    }
}

/// A `METHOD_BUFFERED` request buffer.
struct RequestBuffer {
    ptr: *mut u8,
    len: usize,
}

fn retrieve_input(request: WDFREQUEST, min_len: usize) -> Result<RequestBuffer, NTSTATUS> {
    let mut ptr: PVOID = core::ptr::null_mut();
    let mut len = 0usize;
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestRetrieveInputBuffer,
            request,
            min_len,
            &raw mut ptr,
            &raw mut len
        )
    };
    if status < 0 {
        return Err(status);
    }
    Ok(RequestBuffer {
        ptr: ptr.cast(),
        len,
    })
}

fn retrieve_output(request: WDFREQUEST, min_len: usize) -> Result<RequestBuffer, NTSTATUS> {
    let mut ptr: PVOID = core::ptr::null_mut();
    let mut len = 0usize;
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestRetrieveOutputBuffer,
            request,
            min_len,
            &raw mut ptr,
            &raw mut len
        )
    };
    if status < 0 {
        return Err(status);
    }
    Ok(RequestBuffer {
        ptr: ptr.cast(),
        len,
    })
}

/// `IOCTL_CROSEC_XCMD`: input and output are a `CROSEC_COMMAND` header
/// followed by request / response data. On success the output header's
/// `Result` holds the EC result code.
///
/// With `METHOD_BUFFERED` the input and output buffers are the same memory, so
/// the request is copied out before anything is written back.
fn ioctl_xcmd(ctx: &DeviceContext, request: WDFREQUEST) -> Result<(), NTSTATUS> {
    let input = retrieve_input(request, CROSEC_COMMAND_HEADER_LEN)?;
    let output = retrieve_output(request, CROSEC_COMMAND_HEADER_LEN)?;
    let ec = ctx.ec.as_ref().ok_or(STATUS_DEVICE_NOT_READY)?;

    // SAFETY: The input buffer holds at least a header.
    let mut cmd = unsafe { input.ptr.cast::<CrosEcCommand>().read_unaligned() };

    println!(
        "crosecbus: XCMD Request {request:p} Command {:#x} Version {:#x}",
        cmd.command, cmd.version
    );

    // User tried to send/receive too much data. (These limits are crossed
    // over relative to their names, as in the original driver.)
    if input.len > CROSEC_COMMAND_HEADER_LEN + ec.max_insize()
        || output.len > CROSEC_COMMAND_HEADER_LEN + ec.max_outsize()
    {
        return Err(STATUS_BUFFER_OVERFLOW);
    }
    let out_size = cmd.out_size as usize;
    let in_size = cmd.in_size as usize;
    // User tried to send/receive more bytes than they offered in storage.
    if input.len < CROSEC_COMMAND_HEADER_LEN + out_size
        || output.len < CROSEC_COMMAND_HEADER_LEN + in_size
    {
        return Err(STATUS_BUFFER_TOO_SMALL);
    }

    // The EC only sees the low 16 bits of the command, so check those. (The
    // C driver compared the full 32 bits, which could be bypassed by setting
    // high bits.)
    #[allow(clippy::cast_possible_truncation, reason = "see above")]
    let command = cmd.command as u16;

    // I know this seems overprotective, and that I am wielding too much power
    // over you, but I don't think that the Windows driver should let you
    // erase your EC flash. Since the device grants access to all
    // administrators, that would put you one bad apple away from bricking
    // your machine. Sorry. -- DHowett
    //
    // CoolStar agrees: if you need to update your EC, let coreboot update
    // the RW portion.
    if matches!(
        command,
        EC_CMD_FLASH_ERASE | EC_CMD_FLASH_PROTECT | EC_CMD_FLASH_WRITE | EC_CMD_USB_PD_FW_UPDATE
    ) {
        return Err(STATUS_ACCESS_DENIED);
    }

    // Wait our turn if a kernel driver wants to access first.
    let mut tries = 5;
    while ctx.kernel_accesses_waiting.load(Ordering::SeqCst) != 0 {
        if tries == 0 {
            println!("crosecbus: XCMD timeout waiting for kernel clients");
            return Err(STATUS_RETRY); // Userland should retry later.
        }
        // Wait 500 microseconds before checking again to hopefully let the
        // kernel complete.
        delay_us(500, true);
        tries -= 1;
    }

    // Both sizes are bounded by the checks above (at most 0xFC).
    let mut buf = [0u8; EC_LPC_HOST_PACKET_SIZE];
    // SAFETY: The input buffer holds header + out_size bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(
            input.ptr.add(CROSEC_COMMAND_HEADER_LEN),
            buf.as_mut_ptr(),
            out_size,
        );
    }

    let result = {
        let _guard = WaitLockGuard::acquire(ctx.ec_lock);
        #[allow(clippy::cast_possible_truncation, reason = "as in the C driver")]
        ec.command(command, cmd.version as u8, &mut buf, out_size, in_size)
    };

    println!(
        "crosecbus: XCMD Command {} Version {} OutSize {} Result {:?}",
        cmd.command, cmd.version, cmd.out_size, result
    );

    let received = match result {
        Ok(n) => {
            cmd.result = 0; // 0 = SUCCESS
            n
        }
        Err(CommandError::EcResult(r)) => {
            // Propagate a response code from the EC in Result, and tell the
            // client we received nothing.
            cmd.result = u32::from(r);
            0
        }
        Err(CommandError::Transport(r)) => {
            println!("crosecbus: XCMD failure {r} => {}", res::to_str(r));
            return Err(transport_error_status(r));
        }
    };

    let required_reply_size = CROSEC_COMMAND_HEADER_LEN + received;
    if required_reply_size > output.len {
        println!(
            "crosecbus: XCMD buffer too small. {required_reply_size:#x} > {:#x}",
            output.len
        );
        return Err(STATUS_BUFFER_TOO_SMALL);
    }

    // SAFETY: The output buffer holds at least header + in_size bytes, and
    // received <= in_size.
    unsafe {
        output.ptr.cast::<CrosEcCommand>().write_unaligned(cmd);
        core::ptr::copy_nonoverlapping(
            buf.as_ptr(),
            output.ptr.add(CROSEC_COMMAND_HEADER_LEN),
            received,
        );
        call_unsafe_wdf_function_binding!(
            WdfRequestSetInformation,
            request,
            required_reply_size as u64
        );
    }
    Ok(())
}

/// `IOCTL_CROSEC_RDMEM`: reads the EC memory map.
fn ioctl_read_mem(ctx: &DeviceContext, request: WDFREQUEST) -> Result<(), NTSTATUS> {
    let input = retrieve_input(request, CROSEC_READMEM_LEN)?;
    let output = retrieve_output(request, CROSEC_READMEM_LEN)?;
    let ec = ctx.ec.as_ref().ok_or(STATUS_DEVICE_NOT_READY)?;

    // SAFETY: The input buffer holds at least a CROSEC_READMEM.
    #[allow(clippy::cast_ptr_alignment, reason = "only used with read_unaligned")]
    let (offset, bytes) = unsafe {
        let p = input.ptr.cast::<ULONG>();
        (p.read_unaligned(), p.add(1).read_unaligned())
    };

    if offset.saturating_add(bytes) as usize > CROSEC_MEMMAP_SIZE {
        return Err(STATUS_INVALID_ADDRESS);
    }

    let mut buf = [0u8; CROSEC_MEMMAP_SIZE];
    let read = ec.read_mem(offset as usize, bytes as usize, &mut buf);

    println!("crosecbus: RDMEM Offset {offset:#x} Bytes {bytes} Result {read:?}");

    let read = read.map_err(|()| STATUS_UNSUCCESSFUL)?;

    // SAFETY: The output buffer holds at least a CROSEC_READMEM.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_ptr_alignment,
        reason = "read <= 0xFF; only used with write_unaligned"
    )]
    unsafe {
        let p = output.ptr.cast::<ULONG>();
        p.write_unaligned(offset);
        p.add(1).write_unaligned(read as ULONG);
        core::ptr::copy_nonoverlapping(
            buf.as_ptr(),
            output.ptr.add(CROSEC_READMEM_BUFFER_OFFSET),
            read,
        );
        call_unsafe_wdf_function_binding!(
            WdfRequestSetInformation,
            request,
            CROSEC_READMEM_LEN as u64
        );
    }
    Ok(())
}

extern "C" fn crosec_evt_io_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: ULONG,
) {
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfIoQueueGetDevice, queue) };
    let ctx = unsafe { &*device_get_context(device as WDFOBJECT) };

    let result = match io_control_code {
        IOCTL_CROSEC_XCMD => ioctl_xcmd(ctx, request),
        IOCTL_CROSEC_RDMEM => ioctl_read_mem(ctx, request),
        _ => Err(STATUS_INVALID_PARAMETER),
    };
    let status = result.err().unwrap_or(STATUS_SUCCESS);

    unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestComplete, request, status);
    }
}

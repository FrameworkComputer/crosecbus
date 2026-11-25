use core::sync::atomic::Ordering;

use crosec_proto::{
    commands::{
        ec_host_event_mask,
        EcHostEventMask,
        EcParamsMotionSense,
        EcResponseGetFeatures,
        EcResponseGetNextEvent,
        EcResponseGetVersion,
        EcResponseMotionSense,
        Pod,
        EC_CMD_GET_FEATURES,
        EC_CMD_GET_NEXT_EVENT,
        EC_CMD_GET_VERSION,
        EC_CMD_HOST_EVENT_CLEAR_B,
        EC_CMD_HOST_EVENT_GET_B,
        EC_CMD_MOTION_SENSE_CMD,
        EC_HOST_EVENT_INVALID,
        EC_HOST_EVENT_MKBP,
        EC_MKBP_EVENT_BUTTON,
        MOTIONSENSE_CMD_FIFO_INT_ENABLE,
    },
    Ec,
    ProbeError,
    EC_MEMMAP_SIZE,
};
use wdk::{nt_success, paged_code, println};
use wdk_sys::{
    call_unsafe_wdf_function_binding,
    ntddk::{ExCreateCallback, ExNotifyCallback, ObfDereferenceObject},
    _DEVICE_POWER_STATE,
    _WDF_EXECUTION_LEVEL,
    _WDF_POWER_POLICY_IDLE_TIMEOUT_TYPE,
    _WDF_POWER_POLICY_S0_IDLE_CAPABILITIES,
    _WDF_POWER_POLICY_S0_IDLE_USER_CONTROL,
    _WDF_SYNCHRONIZATION_SCOPE,
    _WDF_TRI_STATE,
    BOOLEAN,
    GUID,
    INTERFACE,
    NTSTATUS,
    OBJECT_ATTRIBUTES,
    PVOID,
    STATUS_CONNECTION_INVALID,
    STATUS_DEVICE_CONFIGURATION_ERROR,
    STATUS_INTERNAL_ERROR,
    STATUS_INVALID_DEVICE_STATE,
    STATUS_INVALID_PARAMETER,
    STATUS_INVALID_PARAMETER_3,
    STATUS_NOINTERFACE,
    STATUS_NO_MEMORY,
    STATUS_SUCCESS,
    ULONG,
    WDFCMRESLIST,
    WDFDEVICE,
    WDFDEVICE_INIT,
    WDFINTERRUPT,
    WDFOBJECT,
    WDFWAITLOCK,
    WDF_DEVICE_POWER_POLICY_IDLE_SETTINGS,
    WDF_DEVICE_STATE,
    WDF_INTERRUPT_CONFIG,
    WDF_NO_HANDLE,
    WDF_NO_OBJECT_ATTRIBUTES,
    WDF_OBJECT_ATTRIBUTES,
    WDF_PNPPOWER_EVENT_CALLBACKS,
    WDF_POWER_DEVICE_STATE,
    WDF_QUERY_INTERFACE_CONFIG,
};

use crate::{
    device_get_context,
    hal::{KernelHal, WaitLockGuard},
    queue::crosecbus_queue_initialize,
    unicode_string,
    wdf_object_context::wdf_get_context_type_info,
    wdf_struct_size,
    CrosEcCommand,
    CrosEcInterfaceStandard,
    CrosEcInterfaceStandardV2,
    CsVivaldiSettingsArg,
    DeviceContext,
    CROSEC_COMMAND_HEADER_LEN,
    CS_VIVALDI_REQUEST_UPDATE_BUTTON,
    GUID_CROSEC_INTERFACE_STANDARD,
    GUID_CROSEC_INTERFACE_STANDARD_V2,
    WDF_DEVICE_CONTEXT_TYPE_INFO,
};

const FILE_DEVICE_CONTROLLER: u32 = 0x0000_0004;
const CM_RESOURCE_TYPE_INTERRUPT: u8 = 2;

const OBJ_PERMANENT: ULONG = 0x0000_0010;
const OBJ_CASE_INSENSITIVE: ULONG = 0x0000_0040;
const OBJ_OPENIF: ULONG = 0x0000_0080;
const OBJ_KERNEL_HANDLE: ULONG = 0x0000_0200;

impl DeviceContext {
    /// Runs `f` under the EC lock on behalf of a kernel client, taking
    /// priority over pending user mode requests.
    fn with_kernel_ec_lock<T>(&self, f: impl FnOnce() -> T) -> T {
        self.kernel_accesses_waiting.fetch_add(1, Ordering::SeqCst);
        let guard = WaitLockGuard::acquire(self.ec_lock);

        let result = f();

        self.kernel_accesses_waiting.fetch_sub(1, Ordering::SeqCst);
        drop(guard);
        result
    }

    /// `send_ec_command`: typed host command from inside this driver.
    fn send_command<P: Pod, R: Pod>(
        &self,
        command: u16,
        version: u8,
        params: &P,
        response: &mut R,
    ) -> NTSTATUS {
        let Some(ec) = self.ec.as_ref() else {
            return STATUS_NOINTERFACE;
        };

        let result =
            self.with_kernel_ec_lock(|| ec.command_typed(command, version, params, response));

        match result {
            Ok(_) => STATUS_SUCCESS,
            Err(e) => {
                println!(
                    "crosecbus: EC command {command:#x} returned error {}",
                    e.legacy_code()
                );
                STATUS_INTERNAL_ERROR
            }
        }
    }

    fn check_feature(&self, feature: i32) -> bool {
        if self.ec_features == [u32::MAX; 2] {
            return false; // Failed to get features
        }
        let Ok(feature) = usize::try_from(feature) else {
            return false;
        };
        self.ec_features
            .get(feature / 32)
            .is_some_and(|word| word & (1 << (feature % 32)) != 0)
    }
}

/// Creates the device and its software resources.
#[link_section = "PAGE"]
pub fn crosecbus_device_create(mut device_init: &mut WDFDEVICE_INIT) -> NTSTATUS {
    paged_code!();

    let mut pnp_power_callbacks = WDF_PNPPOWER_EVENT_CALLBACKS {
        Size: wdf_struct_size!(WDF_PNPPOWER_EVENT_CALLBACKS),
        EvtDevicePrepareHardware: Some(crosecbus_evt_device_prepare_hardware),
        EvtDeviceSelfManagedIoInit: Some(crosecbus_evt_device_self_managed_io_init),
        EvtDeviceSelfManagedIoCleanup: Some(crosecbus_evt_device_self_managed_io_cleanup),
        EvtDeviceD0Entry: Some(crosecbus_evt_device_d0_entry),
        ..WDF_PNPPOWER_EVENT_CALLBACKS::default()
    };
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceInitSetPnpPowerEventCallbacks,
            device_init,
            &raw mut pnp_power_callbacks
        );
    };

    let name = unicode_string!(r"\Device\CrosEC");
    let mut nt_status = unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceInitAssignName, device_init, &raw const name)
    };
    if !nt_success(nt_status) {
        println!("crosecbus: WdfDeviceInitAssignName failed {nt_status:#010X}");
        return nt_status;
    }

    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceInitSetDeviceType,
            device_init,
            FILE_DEVICE_CONTROLLER
        );
    };

    let mut attributes = WDF_OBJECT_ATTRIBUTES {
        Size: wdf_struct_size!(WDF_OBJECT_ATTRIBUTES),
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
        ContextTypeInfo: wdf_get_context_type_info!(DeviceContext),
        ..WDF_OBJECT_ATTRIBUTES::default()
    };

    let mut device = WDF_NO_HANDLE as WDFDEVICE;
    nt_status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreate,
            (core::ptr::addr_of_mut!(device_init)).cast(),
            &raw mut attributes,
            &raw mut device,
        )
    };
    if !nt_success(nt_status) {
        println!("crosecbus: WdfDeviceCreate failed {nt_status:#010X}");
        return nt_status;
    }

    let (ec_lock, mec_lock) = match (create_wait_lock(device), create_wait_lock(device)) {
        (Ok(ec_lock), Ok(mec_lock)) => (ec_lock, mec_lock),
        (Err(nt_status), _) | (_, Err(nt_status)) => return nt_status,
    };

    let device_context = unsafe { device_get_context(device as WDFOBJECT) };
    // SAFETY: WDF allocated the context for `device`; nothing else can
    // reference it yet. Write the whole thing so no field relies on WDF's
    // zero-fill being a valid value.
    unsafe {
        device_context.write(DeviceContext {
            ec_features: [u32::MAX; 2],
            kernel_accesses_waiting: crate::AtomicI64::new(0),
            ec_lock,
            mec_lock,
            interrupt: core::ptr::null_mut(),
            cs_buttons_callback: core::ptr::null_mut(),
            ec: None,
        });
    }

    configure_power_policy(device);

    nt_status = unsafe { crosecbus_queue_initialize(device) };
    if !nt_success(nt_status) {
        println!("crosecbus: queue initialization failed {nt_status:#010X}");
        return nt_status;
    }

    let dos_device_name = unicode_string!(r"\DosDevices\GOOG0004");
    nt_status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreateSymbolicLink,
            device,
            &raw const dos_device_name
        )
    };
    if !nt_success(nt_status) {
        println!("crosecbus: WdfDeviceCreateSymbolicLink failed {nt_status:#010X}");
        return nt_status;
    }

    add_crosec_interfaces(device, device_context.cast())
}

/// Creates a wait lock parented to `device`.
#[link_section = "PAGE"]
fn create_wait_lock(device: WDFDEVICE) -> Result<WDFWAITLOCK, NTSTATUS> {
    paged_code!();

    let mut attributes = WDF_OBJECT_ATTRIBUTES {
        Size: wdf_struct_size!(WDF_OBJECT_ATTRIBUTES),
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
        ParentObject: device as WDFOBJECT,
        ..WDF_OBJECT_ATTRIBUTES::default()
    };
    let mut lock: WDFWAITLOCK = core::ptr::null_mut();
    let nt_status = unsafe {
        call_unsafe_wdf_function_binding!(WdfWaitLockCreate, &raw mut attributes, &raw mut lock)
    };
    if !nt_success(nt_status) {
        println!("crosecbus: WdfWaitLockCreate failed {nt_status:#010X}");
        return Err(nt_status);
    }
    Ok(lock)
}

/// Idle in S0 like the C driver, and allow the device to be disabled.
#[link_section = "PAGE"]
fn configure_power_policy(device: WDFDEVICE) {
    paged_code!();

    let mut idle_settings = WDF_DEVICE_POWER_POLICY_IDLE_SETTINGS {
        Size: wdf_struct_size!(WDF_DEVICE_POWER_POLICY_IDLE_SETTINGS),
        IdleCaps: _WDF_POWER_POLICY_S0_IDLE_CAPABILITIES::IdleCanWakeFromS0,
        DxState: _DEVICE_POWER_STATE::PowerDeviceMaximum,
        IdleTimeout: 1000,
        UserControlOfIdleSettings:
            _WDF_POWER_POLICY_S0_IDLE_USER_CONTROL::IdleDoNotAllowUserControl,
        Enabled: _WDF_TRI_STATE::WdfUseDefault,
        PowerUpIdleDeviceOnSystemWake: _WDF_TRI_STATE::WdfUseDefault,
        IdleTimeoutType: _WDF_POWER_POLICY_IDLE_TIMEOUT_TYPE::SystemManagedIdleTimeoutWithHint,
        ExcludeD3Cold: _WDF_TRI_STATE::WdfUseDefault,
    };
    // Like the C driver, failure here is not fatal.
    let _ = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceAssignS0IdleSettings,
            device,
            &raw mut idle_settings
        )
    };

    let mut device_state = WDF_DEVICE_STATE {
        Size: wdf_struct_size!(WDF_DEVICE_STATE),
        Disabled: _WDF_TRI_STATE::WdfUseDefault,
        DontDisplayInUI: _WDF_TRI_STATE::WdfUseDefault,
        Failed: _WDF_TRI_STATE::WdfUseDefault,
        NotDisableable: _WDF_TRI_STATE::WdfFalse,
        Removed: _WDF_TRI_STATE::WdfUseDefault,
        ResourcesChanged: _WDF_TRI_STATE::WdfUseDefault,
        AssignedToGuest: _WDF_TRI_STATE::WdfUseDefault,
    };
    unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceSetDeviceState, device, &raw mut device_state);
    };
}

/// Exposes the v1 and v2 kernel interfaces to child / filter drivers.
#[link_section = "PAGE"]
fn add_crosec_interfaces(device: WDFDEVICE, context: PVOID) -> NTSTATUS {
    paged_code!();

    let mut v1 = CrosEcInterfaceStandard {
        interface_header: interface_header::<CrosEcInterfaceStandard>(1, context),
        cmd_xfer_status: Some(crosec_cmd_xfer_status),
        check_features: Some(crosec_check_features),
    };
    let nt_status = add_query_interface(
        device,
        (&raw mut v1).cast(),
        &GUID_CROSEC_INTERFACE_STANDARD,
    );
    if !nt_success(nt_status) {
        return nt_status;
    }

    let mut v2 = CrosEcInterfaceStandardV2 {
        interface_header: interface_header::<CrosEcInterfaceStandardV2>(2, context),
        cmd_xfer_status: Some(crosec_cmd_xfer_status),
        check_features: Some(crosec_check_features),
        read_ec_mem: Some(crosec_read_ec_mem),
    };
    add_query_interface(
        device,
        (&raw mut v2).cast(),
        &GUID_CROSEC_INTERFACE_STANDARD_V2,
    )
}

fn interface_header<T>(version: u16, context: PVOID) -> INTERFACE {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "interface structures are a few dozen bytes"
    )]
    INTERFACE {
        Size: core::mem::size_of::<T>() as u16,
        Version: version,
        Context: context,
        // Let the framework handle reference counting.
        InterfaceReference: Some(interface_reference_noop),
        InterfaceDereference: Some(interface_reference_noop),
    }
}

/// Equivalent of `WdfDeviceInterfaceReferenceNoOp`.
const unsafe extern "C" fn interface_reference_noop(_context: PVOID) {}

#[link_section = "PAGE"]
fn add_query_interface(device: WDFDEVICE, interface: *mut INTERFACE, guid: &GUID) -> NTSTATUS {
    paged_code!();

    let mut qi_config = WDF_QUERY_INTERFACE_CONFIG {
        Size: wdf_struct_size!(WDF_QUERY_INTERFACE_CONFIG),
        Interface: interface,
        InterfaceType: guid,
        SendQueryToParentStack: 0,
        EvtDeviceProcessQueryInterfaceRequest: None,
        ImportInterface: 0,
    };
    let nt_status = unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceAddQueryInterface, device, &raw mut qi_config)
    };
    if !nt_success(nt_status) {
        println!("crosecbus: WdfDeviceAddQueryInterface failed {nt_status:#010X}");
    }
    nt_status
}

/// Interface `CmdXferStatus`. `msg` is followed by `max(OutSize, InSize)`
/// bytes of data; `InSize` is clamped to what the EC supports.
unsafe extern "C" fn crosec_cmd_xfer_status(context: PVOID, msg: *mut CrosEcCommand) -> NTSTATUS {
    // SAFETY: `context` is the device context we put in the interface.
    let ctx = unsafe { &*context.cast::<DeviceContext>() };
    if msg.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let Some(ec) = ctx.ec.as_ref() else {
        return STATUS_NOINTERFACE;
    };

    // SAFETY: The caller provides a valid CROSEC_COMMAND. Access goes through
    // the raw pointer since the data extends past the header.
    let header = unsafe { msg.read_unaligned() };
    let mut in_size = header.in_size as usize;
    let out_size = header.out_size as usize;

    if in_size > ec.max_insize() {
        println!("crosecbus: Clamping message receive buffer");
        in_size = ec.max_insize();
        #[allow(clippy::cast_possible_truncation, reason = "max_insize <= 0xFC")]
        unsafe {
            (*msg).in_size = in_size as u32;
        }
    }

    if out_size > ec.max_outsize() {
        println!(
            "crosecbus: request of size {out_size} is too big (max: {})",
            ec.max_outsize()
        );
        return STATUS_INVALID_PARAMETER_3;
    }

    // SAFETY: Per the interface contract the data area holds
    // max(OutSize, InSize) bytes.
    let data = unsafe {
        core::slice::from_raw_parts_mut(
            msg.cast::<u8>().add(CROSEC_COMMAND_HEADER_LEN),
            out_size.max(in_size),
        )
    };

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the C driver truncates the same way"
    )]
    let result = ctx.with_kernel_ec_lock(|| {
        ec.command(
            header.command as u16,
            header.version as u8,
            data,
            out_size,
            in_size,
        )
    });
    match result {
        Ok(_) => STATUS_SUCCESS,
        Err(e) => {
            println!("crosecbus: EC Returned Error: {}", e.legacy_code());
            STATUS_INTERNAL_ERROR
        }
    }
}

/// Interface `CheckFeatures`.
unsafe extern "C" fn crosec_check_features(context: PVOID, feature: i32) -> BOOLEAN {
    // SAFETY: `context` is the device context we put in the interface.
    let ctx = unsafe { &*context.cast::<DeviceContext>() };
    BOOLEAN::from(ctx.check_feature(feature))
}

/// Interface `ReadEcMem` (v2 only). Returns the number of bytes read or -1.
unsafe extern "C" fn crosec_read_ec_mem(
    context: PVOID,
    offset: i32,
    bytes: i32,
    dest: PVOID,
) -> i32 {
    // SAFETY: `context` is the device context we put in the interface.
    let ctx = unsafe { &*context.cast::<DeviceContext>() };
    let Some(ec) = ctx.ec.as_ref() else {
        return -1;
    };
    let (Ok(offset), Ok(bytes)) = (usize::try_from(offset), usize::try_from(bytes)) else {
        return -1;
    };
    if dest.is_null() || offset >= EC_MEMMAP_SIZE || bytes > EC_MEMMAP_SIZE {
        return -1;
    }

    // The most the C implementation could write: `bytes`, or for a string
    // read everything up to the end of the memory map.
    let len = if bytes > 0 {
        bytes
    } else {
        EC_MEMMAP_SIZE - offset
    };
    // SAFETY: The caller provides a buffer large enough for the read.
    let dest = unsafe { core::slice::from_raw_parts_mut(dest.cast::<u8>(), len) };

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "at most EC_MEMMAP_SIZE"
    )]
    ec.read_mem(offset, bytes, dest).map_or(-1, |n| n as i32)
}

/// Finds the interrupt resource, probes the EC and reads its version and
/// feature flags.
extern "C" fn crosecbus_evt_device_prepare_hardware(
    device: WDFDEVICE,
    resources_raw: WDFCMRESLIST,
    resources_translated: WDFCMRESLIST,
) -> NTSTATUS {
    let device_context = unsafe { device_get_context(device as WDFOBJECT) };

    let resource_count = unsafe {
        call_unsafe_wdf_function_binding!(WdfCmResourceListGetCount, resources_translated)
    };
    for i in 0..resource_count {
        let descriptor = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfCmResourceListGetDescriptor,
                resources_translated,
                i
            )
        };
        let descriptor_raw = unsafe {
            call_unsafe_wdf_function_binding!(WdfCmResourceListGetDescriptor, resources_raw, i)
        };
        if descriptor.is_null() || unsafe { (*descriptor).Type } != CM_RESOURCE_TYPE_INTERRUPT {
            continue;
        }

        // Create an interrupt object for hardware notifications.
        let mut interrupt_config = WDF_INTERRUPT_CONFIG {
            Size: wdf_struct_size!(WDF_INTERRUPT_CONFIG),
            ShareVector: _WDF_TRI_STATE::WdfUseDefault,
            EvtInterruptIsr: Some(crosecbus_evt_interrupt_isr),
            PassiveHandling: 1,
            InterruptRaw: descriptor_raw,
            InterruptTranslated: descriptor,
            ReportInactiveOnPowerDown: _WDF_TRI_STATE::WdfUseDefault,
            ..WDF_INTERRUPT_CONFIG::default()
        };
        let mut interrupt: WDFINTERRUPT = core::ptr::null_mut();
        let nt_status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfInterruptCreate,
                device,
                &raw mut interrupt_config,
                WDF_NO_OBJECT_ATTRIBUTES,
                &raw mut interrupt
            )
        };
        if nt_success(nt_status) {
            unsafe { (*device_context).interrupt = interrupt };
            println!("crosecbus: Found Sync GPIO!");
        } else {
            println!("crosecbus: Error creating WDF interrupt object - {nt_status:#010X}");
        }
    }

    // SAFETY: This driver owns the EC's LPC ports, and `mec_lock` is parented
    // to the device, which outlives the context.
    let hal = unsafe { KernelHal::new((*device_context).mec_lock) };
    let ec = match Ec::probe(hal) {
        Ok(ec) => ec,
        Err(e) => {
            println!("crosecbus: EC probe failed: {e:?}");
            return match e {
                ProbeError::NotPresent => STATUS_CONNECTION_INVALID,
                // This results in "{Not Enough Quota}" in Device Manager, as
                // with the C driver.
                ProbeError::NoMemoryMap => STATUS_NO_MEMORY,
                ProbeError::UnsupportedProtocol => STATUS_INVALID_DEVICE_STATE,
            };
        }
    };
    // SAFETY: No I/O or interface calls are in flight before the device has
    // started.
    unsafe { (*device_context).ec = Some(ec) };
    let ctx = unsafe { &mut *device_context };

    let mut version = EcResponseGetVersion::zeroed();
    if !nt_success(ctx.send_command(EC_CMD_GET_VERSION, 0, &[0u8; 0], &mut version)) {
        println!("crosecbus: Error: Could not get version");
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    println!(
        "crosecbus: EC RO Version: {}",
        c_str(&version.version_string_ro)
    );
    println!(
        "crosecbus: EC RW Version: {}",
        c_str(&version.version_string_rw)
    );

    let mut features = EcResponseGetFeatures::zeroed();
    if nt_success(ctx.send_command(EC_CMD_GET_FEATURES, 0, &[0u8; 0], &mut features)) {
        ctx.ec_features = features.flags;
        let [f0, f1] = ctx.ec_features;
        println!("crosecbus: EC Features: {f0:08x} {f1:08x}");
    } else {
        println!("crosecbus: Warning: Couldn't get device features");
        ctx.ec_features = [u32::MAX; 2];
    }

    STATUS_SUCCESS
}

fn c_str(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..end]).unwrap_or("<invalid>")
}

/// Opens (or creates) `\Callback\CsKeyboardSettingsCallbackAPI`, used to
/// forward MKBP button events to `CoolStar`'s keyboard drivers.
extern "C" fn crosecbus_evt_device_self_managed_io_init(device: WDFDEVICE) -> NTSTATUS {
    let device_context = unsafe { device_get_context(device as WDFOBJECT) };

    let mut name = unicode_string!(r"\CallBack\CsKeyboardSettingsCallbackAPI");
    let mut attributes = OBJECT_ATTRIBUTES {
        Length: wdf_struct_size!(OBJECT_ATTRIBUTES),
        RootDirectory: core::ptr::null_mut(),
        ObjectName: &raw mut name,
        Attributes: OBJ_KERNEL_HANDLE | OBJ_OPENIF | OBJ_CASE_INSENSITIVE | OBJ_PERMANENT,
        SecurityDescriptor: core::ptr::null_mut(),
        SecurityQualityOfService: core::ptr::null_mut(),
    };

    unsafe {
        ExCreateCallback(
            &raw mut (*device_context).cs_buttons_callback,
            &raw mut attributes,
            1,
            1,
        )
    }
}

extern "C" fn crosecbus_evt_device_self_managed_io_cleanup(device: WDFDEVICE) {
    let device_context = unsafe { device_get_context(device as WDFOBJECT) };
    unsafe {
        let callback = (*device_context).cs_buttons_callback;
        if !callback.is_null() {
            ObfDereferenceObject(callback.cast());
            (*device_context).cs_buttons_callback = core::ptr::null_mut();
        }
    }
}

/// Disables the motion sensor FIFO interrupt, which this driver doesn't
/// service.
extern "C" fn crosecbus_evt_device_d0_entry(
    device: WDFDEVICE,
    _previous_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    let ctx = unsafe { &*device_get_context(device as WDFOBJECT) };

    let params = EcParamsMotionSense {
        cmd: MOTIONSENSE_CMD_FIFO_INT_ENABLE,
        enable: 0,
        reserved: [0; 10],
    };
    let mut response = EcResponseMotionSense::zeroed();
    // Ignore the result, as the device may not have sensors.
    let _ = ctx.send_command(EC_CMD_MOTION_SENSE_CMD, 1, &params, &mut response);

    STATUS_SUCCESS
}

/// Passive-level ISR: forwards MKBP button events to the keyboard settings
/// callback.
extern "C" fn crosecbus_evt_interrupt_isr(interrupt: WDFINTERRUPT, _message_id: ULONG) -> BOOLEAN {
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfInterruptGetDevice, interrupt) };
    let ctx = unsafe { &*device_get_context(device as WDFOBJECT) };
    BOOLEAN::from(handle_interrupt(ctx))
}

fn handle_interrupt(ctx: &DeviceContext) -> bool {
    let mkbp_mask = ec_host_event_mask(EC_HOST_EVENT_MKBP);

    let mut events = EcHostEventMask::zeroed();
    if !nt_success(ctx.send_command(EC_CMD_HOST_EVENT_GET_B, 0, &[0u8; 0], &mut events)) {
        return false;
    }
    let mask = events.mask;

    if mask & ec_host_event_mask(EC_HOST_EVENT_INVALID) != 0 || mask & mkbp_mask == 0 {
        return false;
    }

    let clear = EcHostEventMask { mask: mkbp_mask };
    if !nt_success(ctx.send_command(EC_CMD_HOST_EVENT_CLEAR_B, 0, &clear, &mut [0u8; 0])) {
        return false;
    }

    let mut event = EcResponseGetNextEvent::zeroed();
    if !nt_success(ctx.send_command(EC_CMD_GET_NEXT_EVENT, 0, &[0u8; 0], &mut event)) {
        return false;
    }

    if event.event_type == EC_MKBP_EVENT_BUTTON && !ctx.cs_buttons_callback.is_null() {
        #[allow(clippy::cast_possible_truncation, reason = "matches the C driver")]
        let mut arg = CsVivaldiSettingsArg {
            arg_sz: core::mem::size_of::<CsVivaldiSettingsArg>() as u32,
            settings_request: CS_VIVALDI_REQUEST_UPDATE_BUTTON,
            button: event.buttons() as u8,
        };
        unsafe {
            ExNotifyCallback(
                ctx.cs_buttons_callback.cast(),
                (&raw mut arg).cast(),
                core::ptr::null_mut(),
            );
        }
    }
    true
}

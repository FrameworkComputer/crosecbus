use wdk::{nt_success, paged_code, println};
use wdk_sys::{
    call_unsafe_wdf_function_binding,
    DRIVER_OBJECT,
    NTSTATUS,
    PCUNICODE_STRING,
    PDRIVER_OBJECT,
    PWDFDEVICE_INIT,
    WDFDRIVER,
    WDF_DRIVER_CONFIG,
    WDF_NO_HANDLE,
    WDF_NO_OBJECT_ATTRIBUTES,
};

use crate::{device, wdf_struct_size};

/// `DriverEntry` initializes the driver and is the first routine called by the
/// system after the driver is loaded.
#[link_section = "INIT"]
#[export_name = "DriverEntry"] // WDF expects a symbol with the name DriverEntry
extern "system" fn driver_entry(
    driver: &mut DRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    println!("crosecbus: DriverEntry");

    let mut driver_config = WDF_DRIVER_CONFIG {
        Size: wdf_struct_size!(WDF_DRIVER_CONFIG),
        EvtDriverDeviceAdd: Some(crosecbus_evt_device_add),
        ..WDF_DRIVER_CONFIG::default()
    };
    let driver_handle_output = WDF_NO_HANDLE.cast::<WDFDRIVER>();

    let nt_status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverCreate,
            driver as PDRIVER_OBJECT,
            registry_path,
            WDF_NO_OBJECT_ATTRIBUTES,
            &raw mut driver_config,
            driver_handle_output,
        )
    };

    if !nt_success(nt_status) {
        println!("crosecbus: WdfDriverCreate failed {nt_status:#010X}");
    }

    nt_status
}

/// Called by the framework in response to `AddDevice` from the `PnP`
/// manager.
#[link_section = "PAGE"]
extern "C" fn crosecbus_evt_device_add(
    _driver: WDFDRIVER,
    device_init: PWDFDEVICE_INIT,
) -> NTSTATUS {
    paged_code!();

    println!("crosecbus: EvtDeviceAdd");

    let device_init =
        // SAFETY: WDF should always be providing a pointer that is properly aligned, dereferencable per https://doc.rust-lang.org/std/ptr/index.html#safety, and initialized. For the lifetime of the resulting reference, the pointed-to memory is never accessed through any other pointer.
        unsafe {
        device_init
            .as_mut()
            .expect("WDF should never provide a null pointer for device_init")
    };
    device::crosecbus_device_create(device_init)
}

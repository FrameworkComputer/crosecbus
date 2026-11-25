//! Kernel implementation of [`crosec_proto::Hal`]: x86 port I/O, kernel
//! delays and a WDF wait lock for the MEC EMI window.

use core::{arch::asm, fmt};

use wdk::println;
use wdk_sys::{
    call_unsafe_wdf_function_binding,
    ntddk::{KeDelayExecutionThread, KeQuerySystemTimePrecise},
    _MODE,
    BOOLEAN,
    KPROCESSOR_MODE,
    LARGE_INTEGER,
    WDFWAITLOCK,
};

pub struct KernelHal {
    mec_lock: WDFWAITLOCK,
}

impl KernelHal {
    /// # Safety
    ///
    /// The caller must own the Chrome EC LPC I/O ports (0x200, 0x204,
    /// 0x800-0x8FF, 0xE00-0xEFF), and `mec_lock` must outlive the returned
    /// value.
    pub const unsafe fn new(mec_lock: WDFWAITLOCK) -> Self {
        Self { mec_lock }
    }
}

pub struct WaitLockGuard(WDFWAITLOCK);

impl WaitLockGuard {
    pub fn acquire(lock: WDFWAITLOCK) -> Self {
        // SAFETY: `lock` is a valid wait lock and we are at PASSIVE_LEVEL.
        // With no timeout the acquire cannot fail.
        unsafe {
            let _ =
                call_unsafe_wdf_function_binding!(WdfWaitLockAcquire, lock, core::ptr::null_mut());
        }
        Self(lock)
    }
}

impl Drop for WaitLockGuard {
    fn drop(&mut self) {
        // SAFETY: We acquired the lock in `acquire`.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfWaitLockRelease, self.0);
        }
    }
}

/// Sleeps for at least `us` microseconds. Must be called at `PASSIVE_LEVEL`.
pub fn delay_us(us: u32, alertable: bool) {
    let mut interval = LARGE_INTEGER {
        // Negative means relative, in 100ns units.
        QuadPart: -10 * i64::from(us),
    };
    #[allow(clippy::cast_possible_truncation, reason = "KernelMode is 0")]
    let mode = _MODE::KernelMode as KPROCESSOR_MODE;
    // SAFETY: `interval` is valid for the duration of the call.
    unsafe {
        let _ = KeDelayExecutionThread(mode, BOOLEAN::from(alertable), &raw mut interval);
    }
}

impl crosec_proto::Hal for KernelHal {
    type Guard<'a> = WaitLockGuard;

    fn inb(&self, port: u16) -> u8 {
        let value: u8;
        // SAFETY: Port ownership is guaranteed by `KernelHal::new`.
        unsafe {
            asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
        }
        value
    }

    fn outb(&self, port: u16, value: u8) {
        // SAFETY: Port ownership is guaranteed by `KernelHal::new`.
        unsafe {
            asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
        }
    }

    fn inw(&self, port: u16) -> u16 {
        let value: u16;
        // SAFETY: Port ownership is guaranteed by `KernelHal::new`.
        unsafe {
            asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags));
        }
        value
    }

    fn outw(&self, port: u16, value: u16) {
        // SAFETY: Port ownership is guaranteed by `KernelHal::new`.
        unsafe {
            asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
        }
    }

    fn delay_us(&self, us: u32) {
        delay_us(us, false);
    }

    fn now_us(&self) -> u64 {
        let mut now = LARGE_INTEGER::default();
        // SAFETY: `now` is valid for the duration of the call.
        unsafe { KeQuerySystemTimePrecise(&raw mut now) };
        // SAFETY: Every bit pattern of the union is a valid i64.
        #[allow(clippy::cast_sign_loss, reason = "system time is positive")]
        let ticks = unsafe { now.QuadPart } as u64;
        ticks / 10
    }

    fn lock_mec(&self) -> WaitLockGuard {
        WaitLockGuard::acquire(self.mec_lock)
    }

    fn log(&self, args: fmt::Arguments<'_>) {
        println!("crosecbus: {args}");
    }
}

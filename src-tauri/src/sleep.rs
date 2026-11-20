use crate::model::Result;

#[cfg(target_os = "macos")]
mod platform {
    use super::Result;
    use std::{ffi::c_void, ptr};

    type CFStringRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            string: *const std::ffi::c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(value: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            level: u32,
            name: CFStringRef,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }

    pub struct SleepInhibitor(u32);

    impl SleepInhibitor {
        pub fn acquire() -> Result<Self> {
            // SAFETY: Both strings are valid UTF-8 C strings; Core Foundation owns the results.
            unsafe {
                let kind = CFStringCreateWithCString(
                    ptr::null(),
                    c"PreventUserIdleSystemSleep".as_ptr(),
                    0x0800_0100,
                );
                if kind.is_null() {
                    return Err(
                        "Could not request sleep prevention: cannot create assertion type.".into(),
                    );
                }
                let name = CFStringCreateWithCString(
                    ptr::null(),
                    c"Singulo photo import".as_ptr(),
                    0x0800_0100,
                );
                if name.is_null() {
                    CFRelease(kind);
                    return Err(
                        "Could not request sleep prevention: cannot create assertion name.".into(),
                    );
                }
                let mut id = 0;
                let result = IOPMAssertionCreateWithName(kind, 255, name, &mut id);
                CFRelease(name);
                CFRelease(kind);
                if result != 0 {
                    return Err(format!(
                        "Could not prevent computer sleep (macOS error {result})."
                    ));
                }
                Ok(Self(id))
            }
        }
    }

    impl Drop for SleepInhibitor {
        fn drop(&mut self) {
            // SAFETY: This assertion ID was created successfully and is released exactly once.
            let result = unsafe { IOPMAssertionRelease(self.0) };
            if result != 0 {
                eprintln!("Could not release the macOS sleep assertion: {result}");
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::Result;
    use std::{marker::PhantomData, rc::Rc};
    use windows::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, EXECUTION_STATE, SetThreadExecutionState,
    };

    pub struct SleepInhibitor {
        previous: EXECUTION_STATE,
        _same_thread: PhantomData<Rc<()>>,
    }

    impl SleepInhibitor {
        pub fn acquire() -> Result<Self> {
            // SAFETY: This affects only the calling thread; the guard cannot move to another thread.
            let previous = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
            if previous.0 == 0 {
                return Err(format!(
                    "Could not prevent computer sleep: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self {
                previous,
                _same_thread: PhantomData,
            })
        }
    }

    impl Drop for SleepInhibitor {
        fn drop(&mut self) {
            // SAFETY: The guard is dropped on the thread that set the execution state.
            if unsafe { SetThreadExecutionState(ES_CONTINUOUS | self.previous) }.0 == 0 {
                eprintln!(
                    "Could not restore the Windows sleep state: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::Result;

    pub struct SleepInhibitor;

    impl SleepInhibitor {
        pub fn acquire() -> Result<Self> {
            Err("Preventing sleep during imports is not supported on this platform.".into())
        }
    }
}

pub use platform::SleepInhibitor;

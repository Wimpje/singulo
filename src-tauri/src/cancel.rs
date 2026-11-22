use crate::model::Result;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(windows)]
use std::{
    os::windows::io::OwnedHandle,
    sync::{Arc, Mutex},
};

#[cfg(windows)]
type SourceThread = Arc<Mutex<Option<OwnedHandle>>>;

pub const CANCELLED: &str = "Import cancelled.";

#[derive(Default)]
pub struct Cancellation {
    requested: AtomicBool,
    read_bytes: AtomicU64,
    #[cfg(windows)]
    source_thread: Mutex<Option<SourceThread>>,
}

impl Cancellation {
    /// Counts source bytes read for the import throughput indicator.
    pub fn count_read(&self, bytes: usize) {
        self.read_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn read_bytes(&self) -> u64 {
        self.read_bytes.load(Ordering::Relaxed)
    }

    pub fn requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    pub fn reset(&self) {
        self.requested.store(false, Ordering::SeqCst);
    }

    pub fn request(&self) {
        if self.requested.swap(true, Ordering::SeqCst) {
            return;
        }
        #[cfg(windows)]
        if let Ok(slot) = self.source_thread.lock()
            && let Some(thread) = slot.as_ref()
            && let Err(error) = interrupt_source(Arc::clone(thread))
        {
            eprintln!("Could not interrupt Windows source I/O: {error}");
        }
    }

    pub fn check(&self) -> Result<()> {
        if self.requested() {
            Err(CANCELLED.into())
        } else {
            Ok(())
        }
    }

    pub fn source_io<T>(&self, read: impl FnOnce() -> Result<T>) -> Result<T> {
        #[cfg(windows)]
        let _registration = {
            use std::os::windows::io::FromRawHandle;
            use windows::Win32::System::Threading::{
                GetCurrentThreadId, OpenThread, THREAD_TERMINATE,
            };

            struct Registration<'a>(&'a Mutex<Option<SourceThread>>, SourceThread);
            impl Drop for Registration<'_> {
                fn drop(&mut self) {
                    if let Ok(mut handle) = self.1.lock() {
                        *handle = None;
                    }
                    if let Ok(mut thread) = self.0.lock() {
                        *thread = None;
                    }
                }
            }

            let mut slot = self.source_thread.lock().map_err(|e| e.to_string())?;
            self.check()?;
            debug_assert!(
                slot.is_none(),
                "Source I/O must be sequential and not nested"
            );
            // SAFETY: OpenThread returns a new, owned handle to this live thread.
            match unsafe { OpenThread(THREAD_TERMINATE, false, GetCurrentThreadId()) } {
                Ok(handle) => {
                    let thread = Arc::new(Mutex::new(Some(unsafe {
                        OwnedHandle::from_raw_handle(handle.0)
                    })));
                    *slot = Some(Arc::clone(&thread));
                    Some(Registration(&self.source_thread, thread))
                }
                Err(error) => {
                    eprintln!("Could not register Windows source I/O for cancellation: {error}");
                    None
                }
            }
        };
        self.check()?;
        read().or_else(|error| {
            self.check()?;
            Err(error)
        })
    }
}

#[cfg(windows)]
fn interrupt_source(source_thread: SourceThread) -> std::io::Result<()> {
    use std::{os::windows::io::AsRawHandle, time::Duration};
    use windows::Win32::{
        Foundation::{ERROR_NOT_FOUND, HANDLE},
        System::IO::CancelSynchronousIo,
    };

    std::thread::Builder::new()
        .name("cancel-source-io".into())
        .spawn(move || {
            loop {
                {
                    let Ok(thread) = source_thread.lock() else {
                        break;
                    };
                    let Some(thread) = thread.as_ref() else { break };
                    // The lock keeps the handle valid and prevents the coordinator
                    // from moving on to archive writes while this call targets it.
                    if let Err(error) =
                        unsafe { CancelSynchronousIo(HANDLE(thread.as_raw_handle())) }
                        && error.code() != ERROR_NOT_FOUND.to_hresult()
                    {
                        eprintln!("Could not cancel pending Windows source I/O: {error}");
                        break;
                    }
                }
                // Cancellation can race with the next syscall being submitted.
                std::thread::sleep(Duration::from_millis(50));
            }
        })?;
    Ok(())
}

use crate::model::Result;
use std::path::Path;

#[cfg(not(windows))]
pub fn recycle(path: &Path) -> Result<()> {
    trash::delete(path).map_err(|e| format!("Trash unavailable or failed: {e}"))?;
    if path.exists() {
        return Err("Trash operation did not move the file.".into());
    }
    Ok(())
}

#[cfg(windows)]
pub fn recycle(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            UI::Shell::{
                FOF_NOERRORUI, FOF_SILENT, FOFX_ADDUNDORECORD, FOFX_EARLYFAILURE,
                FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IShellItem,
                SHCreateItemFromParsingName,
            },
        },
        core::PCWSTR,
    };
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() }
        }
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // RECYCLEONDELETE is mandatory: ALLOWUNDO alone permits destructive fallback.
    let result: Result<()> = unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
        let _com = Com;
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        operation
            .SetOperationFlags(
                FOFX_RECYCLEONDELETE
                    | FOFX_ADDUNDORECORD
                    | FOFX_EARLYFAILURE
                    | FOF_NOERRORUI
                    | FOF_SILENT,
            )
            .map_err(|e| e.to_string())?;
        let item: IShellItem =
            SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).map_err(|e| e.to_string())?;
        operation
            .DeleteItem(&item, None)
            .map_err(|e| e.to_string())?;
        operation.PerformOperations().map_err(|e| e.to_string())?;
        if operation
            .GetAnyOperationsAborted()
            .map_err(|e| e.to_string())?
            .as_bool()
        {
            return Err(
                "Recycle Bin operation was aborted; no permanent fallback is allowed.".into(),
            );
        }
        Ok(())
    };
    result?;
    if path.exists() {
        return Err("Recycle Bin is unavailable or the operation was skipped.".into());
    }
    Ok(())
}

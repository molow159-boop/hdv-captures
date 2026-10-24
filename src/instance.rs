//! A named mutex prevents multiple concurrent application instances.
use crate::error::AppError;
#[cfg(windows)]
pub struct Instance(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Instance {
    pub fn acquire() -> Result<Option<Self>, AppError> {
        use windows_sys::Win32::{
            Foundation::*, System::Threading::CreateMutexW, UI::WindowsAndMessaging::*,
        };
        let name: Vec<u16> = "Local\\HDVCaptures".encode_utf16().chain(Some(0)).collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                let title: Vec<u16> = crate::APP_TITLE.encode_utf16().chain(Some(0)).collect();
                let window = FindWindowW(std::ptr::null(), title.as_ptr());
                if window != 0 {
                    ShowWindowAsync(window, SW_RESTORE);
                    SetForegroundWindow(window);
                }
                return Ok(None);
            }
            Ok(Some(Self(handle)))
        }
    }
}
#[cfg(windows)]
impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

//! Windows notification icon, with its own message loop. Only explicit user
//! actions restore the application: automatic collection never steals focus.
#[cfg(windows)]
mod native {
    use eframe::egui::{Context, ViewportCommand};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{
        mem::{size_of, zeroed},
        sync::mpsc,
        thread,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Shell::*, WindowsAndMessaging::*},
    };

    const CALLBACK: u32 = WM_APP + 1;
    const RESTORE: usize = 1;
    const QUIT: usize = 2;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    struct State {
        ctx: Context,
        main_window: HWND,
        icon: NOTIFYICONDATAW,
        taskbar_created: u32,
    }

    impl State {
        fn restore(&self) {
            self.ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            self.ctx
                .send_viewport_cmd(ViewportCommand::Minimized(false));
            self.ctx.send_viewport_cmd(ViewportCommand::Focus);
            // A hidden winit window may no longer receive RedrawRequested.
            // Wake the native window before relying on egui's command queue.
            unsafe {
                ShowWindowAsync(self.main_window, SW_RESTORE);
                SetForegroundWindow(self.main_window);
            }
            self.ctx.request_repaint();
        }

        fn quit(&self) {
            self.ctx.send_viewport_cmd(ViewportCommand::Close);
            unsafe {
                // Let eframe process its normal close path, including cleanup,
                // even when rendering was suspended while hidden or minimized.
                ShowWindowAsync(self.main_window, SW_SHOWNOACTIVATE);
                PostMessageW(self.main_window, WM_CLOSE, 0, 0);
            }
            self.ctx.request_repaint();
        }
    }

    unsafe extern "system" fn procedure(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // The pointer is installed after CreateWindowExW and removed before the
        // stack-owned state goes out of scope on the message-loop thread.
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if !pointer.is_null() {
            let state = &*pointer;
            if message == state.taskbar_created {
                Shell_NotifyIconW(NIM_ADD, &state.icon);
                return 0;
            }
            if message == WM_COMMAND {
                match wparam & 0xffff {
                    RESTORE => state.restore(),
                    QUIT => state.quit(),
                    _ => {}
                }
                return 0;
            }
            if message == CALLBACK {
                match lparam as u32 {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => state.restore(),
                    WM_RBUTTONUP | WM_CONTEXTMENU => {
                        let menu = CreatePopupMenu();
                        if menu != 0 {
                            AppendMenuW(
                                menu,
                                MF_STRING,
                                RESTORE,
                                wide("Afficher HDV Captures").as_ptr(),
                            );
                            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                            AppendMenuW(menu, MF_STRING, QUIT, wide("Quitter").as_ptr());
                            let mut position = zeroed();
                            GetCursorPos(&mut position);
                            SetForegroundWindow(hwnd);
                            TrackPopupMenu(
                                menu,
                                TPM_RIGHTBUTTON,
                                position.x,
                                position.y,
                                0,
                                hwnd,
                                std::ptr::null(),
                            );
                            DestroyMenu(menu);
                            PostMessageW(hwnd, WM_NULL, 0, 0);
                        }
                    }
                    _ => {}
                }
                return 0;
            }
            if message == WM_CLOSE {
                Shell_NotifyIconW(NIM_DELETE, &state.icon);
                DestroyWindow(hwnd);
                return 0;
            }
        }
        if message == WM_DESTROY {
            PostQuitMessage(0);
            return 0;
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }

    pub struct Tray {
        hwnd: HWND,
    }
    impl Tray {
        pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
            let main_window = match cc.window_handle().map_err(|e| e.to_string())?.as_raw() {
                RawWindowHandle::Win32(window) => window.hwnd.get(),
                _ => return Err("La fenêtre Windows est indisponible".into()),
            };
            let ctx = cc.egui_ctx.clone();
            let (sender, receiver) = mpsc::sync_channel(1);
            thread::spawn(move || unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let class = wide("HDVCapturesNotificationWindow");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(procedure),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    ..zeroed()
                };
                RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    instance,
                    std::ptr::null(),
                );
                if hwnd == 0 {
                    let _ = sender.send(Err("Création de l'icône Windows impossible".into()));
                    return;
                }
                let icon_handle = LoadIconW(instance, 1_u16 as usize as *const u16);
                let mut icon: NOTIFYICONDATAW = zeroed();
                icon.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
                icon.hWnd = hwnd;
                icon.uID = 1;
                icon.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
                icon.uCallbackMessage = CALLBACK;
                icon.hIcon = if icon_handle == 0 {
                    LoadIconW(0, IDI_APPLICATION)
                } else {
                    icon_handle
                };
                let tip = wide("HDV Captures — cliquer pour afficher");
                icon.szTip[..tip.len()].copy_from_slice(&tip);
                let mut state = State {
                    ctx,
                    main_window,
                    icon,
                    taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
                };
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut state as *mut State as isize);
                if Shell_NotifyIconW(NIM_ADD, &state.icon) == 0 {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    DestroyWindow(hwnd);
                    let _ = sender.send(Err(
                        "La zone de notification Windows est indisponible".into()
                    ));
                    return;
                }
                if sender.send(Ok(hwnd)).is_err() {
                    Shell_NotifyIconW(NIM_DELETE, &state.icon);
                    DestroyWindow(hwnd);
                    return;
                }
                let mut message = zeroed();
                while GetMessageW(&mut message, 0, 0, 0) > 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                Shell_NotifyIconW(NIM_DELETE, &state.icon);
                UnregisterClassW(class.as_ptr(), instance);
            });
            receiver
                .recv_timeout(Duration::from_secs(3))
                .map_err(|e| e.to_string())?
                .map(|hwnd| Self { hwnd })
        }
    }
    impl Drop for Tray {
        fn drop(&mut self) {
            unsafe {
                PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
            }
        }
    }
}

#[cfg(windows)]
pub use native::Tray;

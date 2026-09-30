use std::ptr::null;
use std::sync::OnceLock;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, FALSE, FILETIME, HANDLE, HWND, LPARAM, POINT, RECT, TRUE, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, ClientToScreen, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, HDC, SelectObject,
};
use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, SetConsoleCtrlHandler};
use windows_sys::Win32::System::Threading::{
    ExitProcess, GetCurrentProcess, GetCurrentThreadId, GetThreadTimes, OpenThread,
    THREAD_QUERY_LIMITED_INFORMATION, TerminateProcess,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnableMenuItem, GWL_STYLE, GetSystemMenu, GetWindowLongPtrW, GetWindowRect, HMENU, IsIconic,
    IsZoomed, MENU_ITEM_FLAGS, MF_BYCOMMAND, MF_ENABLED, MF_GRAYED, PostMessageW, SC_CLOSE,
    SC_MAXIMIZE, SC_MINIMIZE, SC_MOVE, SC_RESTORE, SC_SIZE, SW_RESTORE, SetForegroundWindow,
    ShowWindowAsync, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN, TrackPopupMenuEx,
    WINDOW_STYLE, WM_NULL, WM_SYSCOMMAND, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU,
    WS_THICKFRAME,
};
use windows_sys::core::BOOL;

pub use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
};

/// `STATUS_CONTROL_C_EXIT`, the exit status a Windows console reports for a
/// program stopped by Ctrl+C.
pub const CONTROL_C_EXIT_CODE: u32 = 0xC000_013A;

type ConsoleCtrlCallback = fn(event: u32) -> bool;

static CONSOLE_CTRL_CALLBACK: OnceLock<ConsoleCtrlCallback> = OnceLock::new();

unsafe extern "system" fn console_ctrl_handler(event: u32) -> BOOL {
    match CONSOLE_CTRL_CALLBACK.get() {
        Some(callback) if callback(event) => TRUE,
        _ => FALSE,
    }
}

/// Install a process-wide console control handler. Windows has no SIGINT: it
/// delivers Ctrl+C, Ctrl+Break and console close as console control events.
///
/// The callback runs on a thread created by the console host and receives the
/// raw `CTRL_*_EVENT` code. Returning `false` from it declines the event so the
/// default handler still runs, terminating the process with its usual status.
///
/// Only the first call installs a handler; later calls report failure.
pub fn install_console_ctrl_handler(callback: ConsoleCtrlCallback) -> bool {
    if CONSOLE_CTRL_CALLBACK.set(callback).is_err() {
        return false;
    }
    unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), TRUE) != 0 }
}

/// Send `CTRL_BREAK_EVENT` to a console process group, the one control event a
/// caller can direct at a single group rather than the whole console. Lets a
/// test drive another process through its installed console control handler.
pub fn send_console_ctrl_break(process_group_id: u32) -> bool {
    unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, process_group_id) != 0 }
}

/// End the current process immediately, skipping module teardown.
///
/// This exists because the console's default control handler — the one that
/// runs once every installed handler declines an event — ends the process with
/// `ExitProcess`. `ExitProcess` takes the loader lock *before* it stops the
/// other threads and then runs `DLL_PROCESS_DETACH` for every loaded module,
/// so a process still running renderer, executor and allocator threads can
/// deadlock there and never terminate at all. `TerminateProcess` asks the
/// kernel to tear the process down instead, which needs neither the loader
/// lock nor any module detach routine.
pub fn terminate_current_process(exit_code: u32) -> ! {
    // A hooked or otherwise refused `TerminateProcess` must not leave this
    // thread parked forever with the process still alive: fall back to the
    // teardown that can deadlock rather than to no teardown at all.
    if unsafe { TerminateProcess(GetCurrentProcess(), exit_code) } == 0 {
        unsafe { ExitProcess(exit_code) }
    }
    // Termination is asynchronous for the calling thread, so wait here rather
    // than returning into code that assumed this call never comes back.
    loop {
        std::thread::park();
    }
}

/// A handle to a thread whose accumulated CPU time can be read from any other
/// thread. Used by the UI responsiveness probe to report how busy the main
/// thread is, independent of what the process as a whole is doing.
pub struct ThreadCpuClock(HANDLE);

// SAFETY: the wrapped value is a kernel handle opened with query-only access;
// `GetThreadTimes` and `CloseHandle` are documented as safe to call on it from
// any thread.
unsafe impl Send for ThreadCpuClock {}
unsafe impl Sync for ThreadCpuClock {}

impl ThreadCpuClock {
    /// Open the calling thread so its CPU time can be sampled later, from any
    /// thread. Returns `None` if the handle cannot be opened.
    pub fn for_current_thread() -> Option<Self> {
        let handle = unsafe {
            OpenThread(
                THREAD_QUERY_LIMITED_INFORMATION,
                FALSE,
                GetCurrentThreadId(),
            )
        };
        if handle.is_null() {
            None
        } else {
            Some(Self(handle))
        }
    }

    /// Kernel plus user CPU time the thread has consumed so far.
    pub fn cpu_time(&self) -> Option<Duration> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
        let ok =
            unsafe { GetThreadTimes(self.0, &mut creation, &mut exit, &mut kernel, &mut user) };
        if ok == 0 {
            return None;
        }
        Some(filetime_duration(kernel) + filetime_duration(user))
    }
}

impl Drop for ThreadCpuClock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// `FILETIME` counts 100-nanosecond ticks.
fn filetime_duration(time: FILETIME) -> Duration {
    let ticks = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    Duration::from_nanos(ticks.saturating_mul(100))
}

/// Restore a Win32 window from the maximized state.
pub fn restore_window(hwnd: isize) -> bool {
    let hwnd = hwnd as HWND;
    unsafe { ShowWindowAsync(hwnd, SW_RESTORE) != 0 }
}

/// A window's pixels, row-major RGBA with straight, fully opaque alpha.
pub struct WindowPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

// `PrintWindow` sits in `Win32_Storage_Xps`; declaring it here avoids turning
// that windows-sys feature on (and rebuilding everything that shares the crate).
#[link(name = "user32")]
unsafe extern "system" {
    fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> BOOL;
}

/// `PrintWindow` flag: render the whole window, including DirectComposition
/// and DirectX content that plain `PW_CLIENTONLY`/`BitBlt` capture misses.
const PW_RENDERFULLCONTENT: u32 = 2;

/// Capture a window's own pixels with `PrintWindow`, independent of anything
/// covering it and without touching focus or input. Dev tooling: the window is
/// asked to paint itself into an off-screen DIB.
pub fn capture_window(hwnd: isize) -> Result<WindowPixels, String> {
    let hwnd = hwnd as HWND;
    unsafe {
        if IsIconic(hwnd) != 0 {
            return Err("window is minimized; restore it to capture".into());
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return Err("GetWindowRect failed".into());
        }
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        if width <= 0 || height <= 0 {
            return Err(format!("window has no area ({width}x{height})"));
        }

        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            return Err("CreateCompatibleDC failed".into());
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        let result = if bitmap.is_null() || bits.is_null() {
            Err("CreateDIBSection failed".to_string())
        } else {
            let previous = SelectObject(dc, bitmap);
            let printed = PrintWindow(hwnd, dc, PW_RENDERFULLCONTENT) != 0;
            GdiFlush();
            let pixels = printed.then(|| {
                let len = width as usize * height as usize * 4;
                let mut rgba = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
                for px in rgba.chunks_exact_mut(4) {
                    px.swap(0, 2); // BGRA -> RGBA
                    px[3] = 255;
                }
                WindowPixels {
                    width: width as u32,
                    height: height as u32,
                    rgba,
                }
            });
            SelectObject(dc, previous);
            pixels.ok_or_else(|| "PrintWindow failed".to_string())
        };
        if !bitmap.is_null() {
            DeleteObject(bitmap);
        }
        DeleteDC(dc);
        result
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct SystemMenuState {
    restore: bool,
    move_window: bool,
    size: bool,
    minimize: bool,
    maximize: bool,
    close: bool,
}

fn has_style(style: WINDOW_STYLE, flag: WINDOW_STYLE) -> bool {
    style & flag == flag
}

fn system_menu_state(
    style: WINDOW_STYLE,
    is_minimized: bool,
    is_maximized: bool,
) -> SystemMenuState {
    let has_system_menu = has_style(style, WS_SYSMENU);
    let is_restored = !is_minimized && !is_maximized;
    let can_resize = has_style(style, WS_THICKFRAME);
    let has_minimize = has_style(style, WS_MINIMIZEBOX);
    let has_maximize = has_style(style, WS_MAXIMIZEBOX);

    SystemMenuState {
        restore: has_system_menu && !is_restored,
        move_window: has_system_menu && is_restored,
        size: has_system_menu && can_resize && is_restored,
        minimize: has_system_menu && has_minimize && !is_minimized,
        maximize: has_system_menu && has_maximize && !is_maximized,
        close: has_system_menu,
    }
}

fn enable_menu_item(menu: HMENU, command: u32, enabled: bool) {
    let flags: MENU_ITEM_FLAGS = MF_BYCOMMAND | if enabled { MF_ENABLED } else { MF_GRAYED };
    unsafe {
        let _ = EnableMenuItem(menu, command, flags);
    }
}

fn sync_system_menu_state(hwnd: HWND, menu: HMENU) {
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) as WINDOW_STYLE };
    let state = system_menu_state(style, unsafe { IsIconic(hwnd) != 0 }, unsafe {
        IsZoomed(hwnd) != 0
    });

    enable_menu_item(menu, SC_RESTORE, state.restore);
    enable_menu_item(menu, SC_MOVE, state.move_window);
    enable_menu_item(menu, SC_SIZE, state.size);
    enable_menu_item(menu, SC_MINIMIZE, state.minimize);
    enable_menu_item(menu, SC_MAXIMIZE, state.maximize);
    enable_menu_item(menu, SC_CLOSE, state.close);
}

/// Show the native Win32 system menu for a window at the given client-area position.
pub fn show_window_system_menu(hwnd: isize, x: i32, y: i32) {
    let hwnd = hwnd as HWND;
    let mut position = POINT { x, y };

    unsafe {
        if ClientToScreen(hwnd, &mut position) == 0 {
            return;
        }

        let menu = GetSystemMenu(hwnd, 0);
        if menu.is_null() {
            return;
        }

        sync_system_menu_state(hwnd, menu);
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenuEx(
            menu,
            TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
            position.x,
            position.y,
            hwnd,
            null(),
        ) as usize;

        let _ = PostMessageW(hwnd, WM_NULL, WPARAM::default(), LPARAM::default());
        if command != 0 {
            let _ = PostMessageW(hwnd, WM_SYSCOMMAND, command as WPARAM, LPARAM::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capturing_an_invalid_window_is_an_error() {
        assert!(capture_window(0).is_err());
    }

    #[test]
    fn restored_window_menu_state_enables_move_size_minimize_and_maximize() {
        let style = WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
        let state = system_menu_state(style, false, false);

        assert_eq!(
            state,
            SystemMenuState {
                restore: false,
                move_window: true,
                size: true,
                minimize: true,
                maximize: true,
                close: true,
            }
        );
    }

    #[test]
    fn maximized_window_menu_state_enables_restore_and_minimize_only() {
        let style = WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
        let state = system_menu_state(style, false, true);

        assert_eq!(
            state,
            SystemMenuState {
                restore: true,
                move_window: false,
                size: false,
                minimize: true,
                maximize: false,
                close: true,
            }
        );
    }

    #[test]
    fn minimized_window_menu_state_enables_restore_and_maximize_only() {
        let style = WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
        let state = system_menu_state(style, true, false);

        assert_eq!(
            state,
            SystemMenuState {
                restore: true,
                move_window: false,
                size: false,
                minimize: false,
                maximize: true,
                close: true,
            }
        );
    }
}

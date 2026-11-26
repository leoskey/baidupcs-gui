//! 平台相关能力：窗口置顶／移动／显示隐藏、显示器工作区、全局光标、暂停进程。
//!
//! gpui 0.2 在 Windows 上没有“置顶”和“创建后移动窗口”的接口，这里通过原生窗口句柄实现。
//! 窗口相关能力目前只有 Windows 完整实现；其他平台返回 `None`，界面退化为不吸附、不强制置顶。
//! 坐标一律为物理像素（与 Win32 一致），避免多屏不同缩放时的换算误差。

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn center(&self) -> (i32, i32) {
        ((self.left + self.right) / 2, (self.top + self.bottom) / 2)
    }
}

/// 原生窗口句柄（Windows 为 HWND）。
pub fn native_handle(window: &gpui::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // gpui::Window 自己也有 window_handle()（返回 gpui 句柄），这里显式调用 raw-window-handle 的版本
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(windows)]
mod imp {
    use super::Rect;
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, POINT, RECT};
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
        DwmSetWindowAttribute,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL,
        MONITORINFO, MonitorFromPoint,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SUSPEND_RESUME};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetClientRect, GetCursorPos, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST,
        IsWindowVisible, SW_HIDE, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_CAPTION,
        WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
    };

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSuspendProcess(process: isize) -> i32;
        fn NtResumeProcess(process: isize) -> i32;
    }

    fn hwnd(h: isize) -> HWND {
        h as HWND
    }

    /// 置顶，并去掉系统窗框：gpui 以重叠窗口样式创建（系统会补上标题栏样式），Windows 11 会在顶部画
    /// 白色窗框、在透明区域下垫一层背景，并加描边和圆角。改成无边框弹出窗口后形状完全由 gpui 绘制。
    pub fn set_topmost(h: isize) {
        let none: u32 = DWMWA_COLOR_NONE;
        let square: i32 = DWMWCP_DONOTROUND;
        unsafe {
            let style = GetWindowLongPtrW(hwnd(h), GWL_STYLE) as u32;
            let popup = (style & !(WS_CAPTION | WS_THICKFRAME | WS_SYSMENU)) | WS_POPUP;
            SetWindowLongPtrW(hwnd(h), GWL_STYLE, popup as isize);
            SetWindowPos(
                hwnd(h),
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            DwmSetWindowAttribute(
                hwnd(h),
                DWMWA_BORDER_COLOR as u32,
                (&none as *const u32).cast(),
                4,
            );
            DwmSetWindowAttribute(
                hwnd(h),
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                (&square as *const i32).cast(),
                4,
            );
            SetWindowPos(
                hwnd(h),
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    fn outer(h: isize) -> Option<RECT> {
        let mut r = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        (unsafe { GetWindowRect(hwnd(h), &mut r) } != 0).then_some(r)
    }

    /// 窗口可见内容（客户区）在屏幕上的位置。gpui 的窗口在 Windows 11 上客户区四周
    /// 留有不可见的边框（左右下为边框厚度，上为 1–2px），所以不能直接用窗口外框定位。
    pub fn window_rect(h: isize) -> Option<Rect> {
        let mut c = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let mut origin = POINT { x: 0, y: 0 };
        (unsafe {
            GetClientRect(hwnd(h), &mut c) != 0 && ClientToScreen(hwnd(h), &mut origin) != 0
        })
        .then(|| Rect {
            left: origin.x,
            top: origin.y,
            right: origin.x + c.right,
            bottom: origin.y + c.bottom,
        })
    }

    /// 客户区外框与窗口外框之差：(左, 上, 右, 下)
    fn insets(h: isize) -> Option<(i32, i32, i32, i32)> {
        let (o, c) = (outer(h)?, window_rect(h)?);
        Some((
            c.left - o.left,
            c.top - o.top,
            o.right - c.right,
            o.bottom - c.bottom,
        ))
    }

    /// 把客户区左上角移到 (x, y)。
    pub fn move_to(h: isize, x: i32, y: i32) {
        let Some((l, t, _, _)) = insets(h) else {
            return;
        };
        unsafe {
            SetWindowPos(
                hwnd(h),
                std::ptr::null_mut(),
                x - l,
                y - t,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }

    /// 把客户区设为 r（位置与大小）。
    pub fn set_frame(h: isize, r: Rect) {
        let Some((l, t, rr, b)) = insets(h) else {
            return;
        };
        unsafe {
            SetWindowPos(
                hwnd(h),
                std::ptr::null_mut(),
                r.left - l,
                r.top - t,
                r.width() + l + rr,
                r.height() + t + b,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }

    pub fn work_area(x: i32, y: i32) -> Option<Rect> {
        let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let monitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
        (unsafe { GetMonitorInfoW(monitor, &mut info) } != 0).then(|| {
            let r = info.rcWork;
            Rect {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            }
        })
    }

    /// 窗口当前所在屏幕的缩放比例（跨屏拖动后会变化）。
    pub fn dpi_scale(h: isize) -> Option<f32> {
        let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd(h)) };
        (dpi != 0).then(|| dpi as f32 / 96.0)
    }

    pub fn on_any_screen(x: i32, y: i32) -> bool {
        !unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) }.is_null()
    }

    pub fn is_visible(h: isize) -> bool {
        unsafe { IsWindowVisible(hwnd(h)) != 0 }
    }

    pub fn set_visible(h: isize, show: bool) {
        unsafe { ShowWindow(hwnd(h), if show { SW_SHOWNOACTIVATE } else { SW_HIDE }) };
    }

    pub fn cursor() -> Option<(i32, i32)> {
        let mut p = POINT { x: 0, y: 0 };
        (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x, p.y))
    }

    pub fn left_button_down() -> bool {
        (unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } as u16 & 0x8000) != 0
    }

    /// 冻结／恢复整个进程的全部线程；进程、所在终端与断点状态保持不变。
    pub fn set_suspended(pid: u32, suspend: bool) -> bool {
        let process = unsafe { OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid) };
        if process.is_null() {
            return false;
        }
        let status = unsafe {
            if suspend {
                NtSuspendProcess(process as isize)
            } else {
                NtResumeProcess(process as isize)
            }
        };
        unsafe { CloseHandle(process) };
        status == 0
    }
}

#[cfg(not(windows))]
mod imp {
    // ponytail: macOS／Linux 的窗口置顶与移动未实现（Wayland 不允许客户端定位窗口），胶囊不吸附。
    use super::Rect;
    pub fn set_topmost(_: isize) {}
    pub fn window_rect(_: isize) -> Option<Rect> {
        None
    }
    pub fn move_to(_: isize, _: i32, _: i32) {}
    pub fn set_frame(_: isize, _: Rect) {}
    pub fn work_area(_: i32, _: i32) -> Option<Rect> {
        None
    }
    pub fn dpi_scale(_: isize) -> Option<f32> {
        None
    }
    pub fn on_any_screen(_: i32, _: i32) -> bool {
        false
    }
    pub fn is_visible(_: isize) -> bool {
        true
    }
    pub fn set_visible(_: isize, _: bool) {}
    pub fn cursor() -> Option<(i32, i32)> {
        None
    }
    pub fn left_button_down() -> bool {
        false
    }
    /// SIGSTOP／SIGCONT：与 Windows 的冻结线程等效，进程与终端保持不变。
    pub fn set_suspended(pid: u32, suspend: bool) -> bool {
        unsafe {
            libc::kill(
                pid as libc::pid_t,
                if suspend {
                    libc::SIGSTOP
                } else {
                    libc::SIGCONT
                },
            ) == 0
        }
    }
}

pub use imp::*;

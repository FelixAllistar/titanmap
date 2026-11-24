//! Screen capture and key watching for the desktop overlay (Windows). Plain GDI screen copies
//! and GetAsyncKeyState: nothing touches the game process.

use image::RgbaImage;

/// Copy the monitor under the foreground window (the game) as RGBA.
#[cfg(windows)]
pub fn capture_foreground_monitor() -> Option<RgbaImage> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    unsafe {
        let fg = GetForegroundWindow();
        let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut info) == 0 { return None; }
        let RECT { left, top, right, bottom } = info.rcMonitor;
        let (w, h) = (right - left, bottom - top);
        let screen = GetDC(std::ptr::null_mut());
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp as _);
        let ok = BitBlt(mem, 0, 0, w, h, screen, left, top, SRCCOPY) != 0;
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h; // top-down
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(mem, bmp, 0, h as u32, buf.as_mut_ptr() as _, &mut bi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        DeleteObject(bmp as _);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), screen);
        if !ok || lines == 0 { return None; }
        for px in buf.chunks_exact_mut(4) { px.swap(0, 2); px[3] = 255; } // BGRA -> RGBA
        RgbaImage::from_raw(w as u32, h as u32, buf)
    }
}

#[cfg(not(windows))]
pub fn capture_foreground_monitor() -> Option<RgbaImage> { None }

/// Is this virtual key held right now? (Doesn't consume it: the game still gets the press.)
#[cfg(windows)]
pub fn key_down(vk: i32) -> bool {
    unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk) as u16 & 0x8000 != 0 }
}

#[cfg(not(windows))]
pub fn key_down(_vk: i32) -> bool { false }

/// The process id that owns the foreground window, to skip presses made in our own window.
#[cfg(windows)]
pub fn foreground_pid() -> u32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
    pid
}

#[cfg(not(windows))]
pub fn foreground_pid() -> u32 { 0 }

/// "M" -> 0x4D, "F9" -> 0x78, "Tab" -> 0x09.
pub fn vk_from_name(name: &str) -> Option<i32> {
    let n = name.trim().to_ascii_uppercase();
    if n.len() == 1 {
        let c = n.as_bytes()[0];
        if c.is_ascii_alphanumeric() { return Some(c as i32); }
    }
    if let Some(f) = n.strip_prefix('F').and_then(|d| d.parse::<i32>().ok()).filter(|f| (1..=24).contains(f)) { return Some(0x70 + f - 1); }
    match n.as_str() { "TAB" => Some(0x09), "ESC" | "ESCAPE" => Some(0x1B), _ => None }
}

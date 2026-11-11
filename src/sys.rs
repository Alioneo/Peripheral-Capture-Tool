//! Win32 最小 FFI：窗口、消息循环、时间、打开目录、日志目录判定。
//! 零第三方依赖，类型名刻意与 Win32 SDK 一致，便于对照 MSDN。

#![allow(non_snake_case, non_camel_case_types, dead_code)]
#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};

pub type HWND = isize;
pub type WPARAM = usize;
pub type LPARAM = isize;
pub type LRESULT = isize;
pub type HINSTANCE = isize;
pub type HMENU = isize;
pub type HCURSOR = isize;
pub type HICON = isize;
pub type HBRUSH = isize;

pub const WM_DESTROY: u32 = 0x0002;
pub const WM_SIZE: u32 = 0x0005;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_ERASEBKGND: u32 = 0x0014;
pub const WM_GETMINMAXINFO: u32 = 0x0024;
pub const WM_TIMER: u32 = 0x0113;

pub const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
pub const SW_SHOWNORMAL: i32 = 1;
pub const SW_SHOW: i32 = 5;

pub const SM_CXSCREEN: i32 = 0;
pub const SM_CYSCREEN: i32 = 1;
pub const IDC_ARROW: isize = 32512;

pub const COINIT_APARTMENTTHREADED: u32 = 0x2;
pub const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: isize = -4;

#[repr(C)]
pub struct WndClassW {
    pub style: u32,
    pub lpfn_wnd_proc: isize,
    pub cls_extra: i32,
    pub wnd_extra: i32,
    pub h_instance: HINSTANCE,
    pub h_icon: HICON,
    pub h_cursor: HCURSOR,
    pub hbr_background: HBRUSH,
    pub menu_name: *const u16,
    pub class_name: *const u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Msg {
    pub hwnd: HWND,
    pub message: u32,
    pub w_param: WPARAM,
    pub l_param: LPARAM,
    pub time: u32,
    pub pt: Point,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MinMaxInfo {
    pub pt_reserved: Point,
    pub pt_max_size: Point,
    pub pt_max_position: Point,
    pub pt_min_track: Point,
    pub pt_max_track: Point,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SystemTime {
    pub year: u16,
    pub month: u16,
    pub day_of_week: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millis: u16,
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
    pub fn GetLocalTime(st: *mut SystemTime);
    pub fn CloseHandle(object: isize) -> i32;
    pub fn GetCurrentProcessId() -> u32;
}

#[link(name = "user32")]
extern "system" {
    pub fn RegisterClassW(class: *const WndClassW) -> u16;
    pub fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HWND,
        menu: HMENU,
        instance: HINSTANCE,
        param: *mut c_void,
    ) -> HWND;
    pub fn DefWindowProcW(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT;
    pub fn DestroyWindow(hwnd: HWND) -> i32;
    pub fn GetMessageW(msg: *mut Msg, hwnd: HWND, min: u32, max: u32) -> i32;
    pub fn TranslateMessage(msg: *const Msg) -> i32;
    pub fn DispatchMessageW(msg: *const Msg) -> LRESULT;
    pub fn PostQuitMessage(code: i32);
    pub fn PostMessageW(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> i32;
    pub fn SetTimer(hwnd: HWND, id: usize, elapse: u32, callback: isize) -> usize;
    pub fn KillTimer(hwnd: HWND, id: usize) -> i32;
    pub fn GetClientRect(hwnd: HWND, rect: *mut Rect) -> i32;
    pub fn LoadCursorW(instance: HINSTANCE, name: *const u16) -> HCURSOR;
    pub fn SetForegroundWindow(hwnd: HWND) -> i32;
    pub fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    pub fn GetSystemMetrics(index: i32) -> i32;
    pub fn SetProcessDpiAwarenessContext(context: isize) -> i32;
    pub fn GetDpiForSystem() -> u32;
    pub fn IsWindow(hwnd: HWND) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    pub fn CoInitializeEx(reserved: *const c_void, coinit: u32) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    pub fn ShellExecuteW(
        hwnd: HWND,
        op: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
    ) -> isize;
}

pub fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// `15:30:12.345`
pub fn now_hms() -> String {
    let mut st = SystemTime::default();
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        st.hour, st.minute, st.second, st.millis
    )
}

/// `20261111_153012`
pub fn now_stamp() -> String {
    let mut st = SystemTime::default();
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        st.year, st.month, st.day, st.hour, st.minute, st.second
    )
}

/// `2026-11-11 15:30:12`
pub fn now_pretty() -> String {
    let mut st = SystemTime::default();
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.year, st.month, st.day, st.hour, st.minute, st.second
    )
}

pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 目录是否可写（用临时文件试探，不留下痕迹）。
fn writable(dir: &Path) -> bool {
    let probe = dir.join(".write_test_tmp");
    match std::fs::write(&probe, b"x") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 绿色版：优先 exe 同目录；不可写时退回 %LOCALAPPDATA%\HIDCapture。
pub fn data_dir() -> PathBuf {
    let dir = exe_dir();
    if writable(&dir) {
        return dir;
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.clone())
        .join("HIDCapture");
    let _ = std::fs::create_dir_all(&base);
    base
}

/// 抓包结果目录的根：和日志同策略（exe 旁，不可写则 LOCALAPPDATA）。
pub fn output_root() -> PathBuf {
    data_dir()
}

pub fn open_path(path: &str) {
    let op = wide("open");
    let file = wide(path);
    unsafe {
        ShellExecuteW(
            0,
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

pub fn hide_console_if_release() {
    // release 下由 windows_subsystem="windows" 保证无控制台，无需额外处理。
}

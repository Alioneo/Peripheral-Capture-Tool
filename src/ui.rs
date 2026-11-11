//! 主窗口与 WebView2 界面：窗口生命周期、IPC 分发、状态推送。

use std::cell::RefCell;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::report;
use crate::session::{self, Session};
use crate::sys::{self, HWND};

const CLASS_NAME: &str = "HidCaptureWindowV1";
const TIMER_UI: usize = 1;
const HTML: &str = include_str!("../assets/wizard.html");

thread_local! {
    static WEB: RefCell<Option<wry::WebView>> = const { RefCell::new(None) };
}

pub fn run() {
    unsafe {
        sys::SetProcessDpiAwarenessContext(sys::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        sys::CoInitializeEx(ptr::null(), sys::COINIT_APARTMENTTHREADED);
    }

    let state = Arc::new(Mutex::new(Session::new()));
    let _ = STATE.set(state.clone());
    let exit = Arc::new(AtomicBool::new(false));
    session::spawn_manager(state.clone(), exit.clone());

    unsafe {
        register_class();
        let scale = sys::GetDpiForSystem() as f64 / 96.0;
        let w = (1240.0 * scale) as i32;
        let h = (860.0 * scale) as i32;
        let sw = sys::GetSystemMetrics(sys::SM_CXSCREEN);
        let sh = sys::GetSystemMetrics(sys::SM_CYSCREEN);
        let x = ((sw - w) / 2).max(0);
        let y = ((sh - h) / 2).max(0);

        let class = sys::wide(CLASS_NAME);
        let title = sys::wide("外设抓包工具 v1.0");
        let hinst = sys::GetModuleHandleW(ptr::null());
        let hwnd = sys::CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            sys::WS_OVERLAPPEDWINDOW,
            x,
            y,
            w,
            h,
            0,
            0,
            hinst,
            ptr::null_mut(),
        );
        if hwnd == 0 {
            crate::log::line("主窗口创建失败");
            return;
        }

        let mut rc = sys::Rect::default();
        sys::GetClientRect(hwnd, &mut rc);
        let cw = (rc.right - rc.left).max(800) as u32;
        let ch = (rc.bottom - rc.top).max(600) as u32;

        let mut ctx = wry::WebContext::new(Some(crate::sys::data_dir().join("webview2")));
        let state_ipc = state.clone();
        let builder = wry::WebViewBuilder::new_with_web_context(&mut ctx)
            .with_html(HTML)
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::PhysicalSize::new(cw, ch).into(),
            })
            .with_background_color((244, 246, 244, 255))
            .with_devtools(cfg!(debug_assertions))
            .with_ipc_handler(move |req: wry::http::Request<String>| {
                if !handle_ipc(req.body(), &state_ipc) {
                    sys::PostMessageW(hwnd, sys::WM_CLOSE, 0, 0);
                }
            });

        let wrap = HwndWrap(hwnd);
        match builder.build_as_child(&wrap) {
            Ok(web) => {
                WEB.with(|slot| *slot.borrow_mut() = Some(web));
            }
            Err(e) => {
                crate::log::line(&format!("WebView2 创建失败：{e}"));
                sys::DestroyWindow(hwnd);
                return;
            }
        }

        sys::SetTimer(hwnd, TIMER_UI, 200, 0);
        sys::ShowWindow(hwnd, sys::SW_SHOW);
        sys::SetForegroundWindow(hwnd);
        crate::log::line("主窗口已显示");

        let mut msg = sys::Msg::default();
        loop {
            let r = sys::GetMessageW(&mut msg, 0, 0, 0);
            if r <= 0 {
                break;
            }
            sys::TranslateMessage(&msg);
            sys::DispatchMessageW(&msg);
        }
    }

    exit.store(true, Ordering::Relaxed);
    crate::log::line("退出");
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: sys::WPARAM, lp: sys::LPARAM) -> sys::LRESULT {
    match msg {
        sys::WM_SIZE => {
            let w = (lp as u32 & 0xFFFF) as i32;
            let h = ((lp as u32 >> 16) & 0xFFFF) as i32;
            if w > 0 && h > 0 {
                WEB.with(|slot| {
                    if let Some(web) = slot.borrow().as_ref() {
                        let _ = web.set_bounds(wry::Rect {
                            position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                            size: wry::dpi::PhysicalSize::new(w as u32, h as u32).into(),
                        });
                    }
                });
            }
            0
        }
        sys::WM_TIMER => {
            if wp == TIMER_UI {
                if let Some(st) = STATE.get() {
                    WEB.with(|slot| {
                        if let Some(web) = slot.borrow().as_ref() {
                            let json = match st.lock() {
                                Ok(g) => session::snapshot_json(&g),
                                Err(_) => return,
                            };
                            let _ = web.evaluate_script(&format!(
                                "window.__state && window.__state({json})"
                            ));
                        }
                    });
                }
            }
            0
        }
        sys::WM_GETMINMAXINFO => {
            let info = lp as *mut sys::MinMaxInfo;
            if !info.is_null() {
                (*info).pt_min_track = sys::Point { x: 1040, y: 700 };
            }
            0
        }
        sys::WM_ERASEBKGND => 1,
        sys::WM_CLOSE => {
            sys::DestroyWindow(hwnd);
            0
        }
        sys::WM_DESTROY => {
            sys::KillTimer(hwnd, TIMER_UI);
            WEB.with(|slot| {
                let _ = slot.borrow_mut().take();
            });
            sys::PostQuitMessage(0);
            0
        }
        _ => sys::DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn register_class() {
    let name = sys::wide(CLASS_NAME);
    let wc = sys::WndClassW {
        style: 0,
        lpfn_wnd_proc: wnd_proc as *const () as usize as isize,
        cls_extra: 0,
        wnd_extra: 0,
        h_instance: sys::GetModuleHandleW(ptr::null()),
        h_icon: 0,
        h_cursor: sys::LoadCursorW(0, sys::IDC_ARROW as *const u16),
        hbr_background: 0,
        menu_name: ptr::null(),
        class_name: name.as_ptr(),
    };
    sys::RegisterClassW(&wc);
}

/// 界面定时器需要拿到会话状态（与 WebView 同为 UI 线程）。
static STATE: std::sync::OnceLock<Arc<Mutex<Session>>> = std::sync::OnceLock::new();

struct HwndWrap(HWND);

impl wry::raw_window_handle::HasWindowHandle for HwndWrap {
    fn window_handle(
        &self,
    ) -> Result<wry::raw_window_handle::WindowHandle<'_>, wry::raw_window_handle::HandleError> {
        use wry::raw_window_handle as rwh;
        let hwnd = std::num::NonZeroIsize::new(self.0).ok_or(rwh::HandleError::Unavailable)?;
        let handle = rwh::Win32WindowHandle::new(hwnd);
        Ok(unsafe { rwh::WindowHandle::borrow_raw(rwh::RawWindowHandle::Win32(handle)) })
    }
}

/// 页面 → 宿主。返回 false 表示关闭窗口。
fn handle_ipc(body: &str, state: &Arc<Mutex<Session>>) -> bool {
    let mut it = body.splitn(3, '|');
    let cmd = it.next().unwrap_or("");
    match cmd {
        "ready" => {
            if let Ok(mut s) = state.lock() {
                s.pending_scan = true;
            }
        }
        "close" => return false,
        "rescan" => {
            if let Ok(mut s) = state.lock() {
                s.pending_scan = true;
                session::push_log(&mut s, "重新扫描设备");
            }
        }
        "select" => {
            let keys = it.next().unwrap_or("");
            let auto = it.next().unwrap_or("1") == "1";
            if let Ok(mut s) = state.lock() {
                s.selected.clear();
                for k in keys.split(',') {
                    let k = k.trim().to_ascii_uppercase();
                    if k.len() == 9 && k.as_bytes()[4] == b':' {
                        s.selected.insert(k);
                    }
                }
                s.auto = auto;
                let n = s.selected.len();
                session::push_log(&mut s, &format!("选择已更新：勾选 {n} 组，自动模式 {}", if auto { "开" } else { "关" }));
            }
        }
        "start" => {
            let id: u8 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let plan: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(120);
            if let Ok(mut s) = state.lock() {
                session::start_phase(&mut s, id, plan);
            }
        }
        "stop" => {
            let id: u8 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            if let Ok(mut s) = state.lock() {
                session::stop_phase(&mut s, id);
            }
        }
        "note" => {
            let id: u8 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let text = percent_decode(it.next().unwrap_or(""));
            if let Ok(mut s) = state.lock() {
                session::set_note(&mut s, id, &text);
            }
        }
        "report" => {
            if let Ok(mut s) = state.lock() {
                match report::generate(&mut s) {
                    Ok(out) => s.report = Some(out),
                    Err(e) => session::push_log(&mut s, &format!("生成报告失败：{e}")),
                }
            }
        }
        "open" => {
            let path = percent_decode(it.next().unwrap_or(""));
            if !path.is_empty() {
                sys::open_path(&path);
            }
        }
        _ => {}
    }
    true
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

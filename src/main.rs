//! 外设抓包工具：按向导抓取键鼠 HID 原始帧，导出给 AI 分析。
//!
//! 用法：
//!   HIDCapture.exe              启动图形向导（默认）
//!   HIDCapture.exe --list       命令行列出所有 HID 接口（排障用）
//!   HIDCapture.exe --probe N    命令行抓 N 秒（默认 15），打印结果后退出（排障用）

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hid;
mod log;
mod report;
mod session;
mod sys;
mod ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = sys::data_dir();
    log::init(&dir);
    log::line(&format!("启动，数据目录 {}", dir.display()));

    match args.first().map(|s| s.as_str()) {
        Some("--list") => probe_list(),
        Some("--probe") => {
            let secs: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(15);
            probe_capture(secs);
        }
        _ => ui::run(),
    }
}

fn probe_list() {
    let paths = hid::list_paths();
    println!("HID 接口共 {} 个：", paths.len());
    for p in &paths {
        match hid::inspect(p) {
            Some(i) => println!(
                "  {:04X}:{:04X} COL{:?} usage {:04X}/{:04X} len={}  {}  厂商={}",
                i.vendor_id,
                i.product_id,
                i.collection,
                i.usage_page,
                i.usage,
                i.input_report_len,
                if i.product.is_empty() { "(无名称)" } else { &i.product },
                i.manufacturer
            ),
            None => println!("  (无法解析) {p}"),
        }
    }
}

fn probe_capture(secs: u64) {
    let st = Arc::new(Mutex::new(session::Session::new()));
    session::scan(&st);
    {
        let s = st.lock().unwrap();
        println!(
            "在线接口 {} 个，设备组 {} 个：",
            s.devices.len(),
            s.groups.len()
        );
        for g in s.groups.values() {
            println!(
                "  {}  {:04X}:{:04X}  接口 {}  标签 {:?}",
                g.display_name(),
                g.vid,
                g.pid,
                g.ifaces,
                g.tags
            );
        }
    }

    let exit = Arc::new(AtomicBool::new(false));
    session::spawn_manager(st.clone(), exit.clone());
    {
        let mut s = st.lock().unwrap();
        s.auto = true;
        session::start_phase(&mut s, 2, secs as u32);
    }

    for i in 0..secs {
        std::thread::sleep(Duration::from_secs(1));
        let s = st.lock().unwrap();
        println!(
            "  [{:>2}s] 帧 {}，读数线程 {}，活动步骤 {:?}",
            i + 1,
            s.frames.len(),
            s.running.len(),
            s.active_id()
        );
    }

    {
        let mut s = st.lock().unwrap();
        if let Some(id) = s.active_id() {
            session::stop_phase(&mut s, id);
        }
    }

    let s = st.lock().unwrap();
    println!("\n--- 汇总 ---");
    let mut by_path: std::collections::BTreeMap<&str, Vec<&session::Frame>> =
        std::collections::BTreeMap::new();
    for f in &s.frames {
        by_path.entry(f.path.as_str()).or_default().push(f);
    }
    for (path, list) in &by_path {
        println!(
            "{}  共 {} 帧  样例 {}",
            path,
            list.len(),
            list.iter()
                .take(3)
                .map(|f| session::hex(&f.data))
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
    exit.store(true, Ordering::Relaxed);
    std::process::exit(0);
}

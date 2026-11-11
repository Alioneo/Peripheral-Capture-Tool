//! 极简日志：写 exe 同目录（或数据目录）HIDCapture.log，单文件上限 1MB。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

pub fn init(dir: &Path) {
    let path = dir.join("HIDCapture.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 1_000_000 {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
        if let Ok(mut slot) = FILE.lock() {
            *slot = Some(f);
        }
    }
}

pub fn line(msg: &str) {
    if let Ok(mut slot) = FILE.lock() {
        if let Some(f) = slot.as_mut() {
            let _ = writeln!(f, "[{}] {msg}", crate::sys::now_hms());
        }
    }
}

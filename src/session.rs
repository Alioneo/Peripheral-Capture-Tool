//! 抓包会话：设备扫描、常驻读取线程、步骤（阶段）管理、界面状态快照。
//!
//! 设计要点：
//! - 每个 HID 接口一条常驻阻塞读线程；线程只负责收帧，是否落库由「当前步骤 + 作用域」决定。
//! - 去重限流：同一接口连续相同的帧只按 3 秒心跳记录一次；每接口每步最多 500 帧，
//!   每步全局最多 15000 帧，防止 1000Hz 鼠标把内存打满。
//! - 阶段只切换标签，不启停线程，避免切换连接方式时丢帧。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::hid::{self, HidDeviceInfo};

pub const MAX_PHASE_FRAMES: usize = 15_000;
pub const MAX_PATH_FRAMES: usize = 500;

pub fn step_name(id: u8) -> &'static str {
    match id {
        2 => "2.4G 接收器",
        3 => "有线 USB",
        4 => "蓝牙",
        5 => "充电状态",
        6 => "电量变化",
        _ => "未知步骤",
    }
}

#[derive(Clone)]
pub struct Frame {
    pub phase: u8,
    pub t_ms: u64,
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub col: Option<u16>,
    pub usage_page: u16,
    pub usage: u16,
    pub data: Vec<u8>,
}

pub struct Active {
    pub id: u8,
    pub name: String,
    pub started: Instant,
    pub started_at: String,
    pub plan: u32,
    pub count: usize,
    pub path_counts: BTreeMap<String, usize>,
}

pub struct PhaseInfo {
    pub id: u8,
    pub name: String,
    pub plan: u32,
    pub duration_ms: u64,
    pub frames: usize,
    pub started_at: String,
}

#[derive(Clone)]
pub struct Group {
    pub key: String,
    pub vid: u16,
    pub pid: u16,
    pub name: String,
    pub manufacturer: String,
    pub ifaces: usize,
    pub online: bool,
    pub tags: BTreeSet<String>,
}

impl Group {
    fn new(info: &HidDeviceInfo) -> Group {
        let mut g = Group {
            key: info.group_key(),
            vid: info.vendor_id,
            pid: info.product_id,
            name: info.product.clone(),
            manufacturer: info.manufacturer.clone(),
            ifaces: 0,
            online: false,
            tags: BTreeSet::new(),
        };
        g.absorb(info);
        g
    }

    fn absorb(&mut self, info: &HidDeviceInfo) {
        if !info.product.is_empty() {
            self.name = info.product.clone();
        }
        if !info.manufacturer.is_empty() {
            self.manufacturer = info.manufacturer.clone();
        }
        if let Some(tag) = usage_tag(info.usage_page, info.usage) {
            self.tags.insert(tag.to_string());
        }
    }

    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else if !self.manufacturer.is_empty() {
            format!("{} 设备", self.manufacturer)
        } else {
            "未命名设备".to_string()
        }
    }
}

pub struct ReportOut {
    pub dir: String,
    pub files: Vec<String>,
}

pub struct Session {
    /// 已知设备组（按 VID:PID 聚合，离线也保留，方便用户勾选）。
    pub groups: BTreeMap<String, Group>,
    /// 当前在线的接口快照（读取线程与作用域判断用）。
    pub devices: Vec<HidDeviceInfo>,
    /// 已打开过的接口属性缓存（路径 → 属性），避免反复 CreateFile。
    pub cache: BTreeMap<String, HidDeviceInfo>,
    pub selected: BTreeSet<String>,
    pub auto: bool,
    pub active: Option<Active>,
    pub phases: Vec<PhaseInfo>,
    pub counts: BTreeMap<u8, usize>,
    pub frames: Vec<Frame>,
    pub notes: BTreeMap<u8, String>,
    pub running: BTreeSet<String>,
    pub report: Option<ReportOut>,
    pub log: Vec<String>,
    pub scan: u64,
    pub pending_scan: bool,
}

impl Session {
    pub fn new() -> Session {
        Session {
            groups: BTreeMap::new(),
            devices: Vec::new(),
            cache: BTreeMap::new(),
            selected: BTreeSet::new(),
            auto: true,
            active: None,
            phases: Vec::new(),
            counts: BTreeMap::new(),
            frames: Vec::new(),
            notes: BTreeMap::new(),
            running: BTreeSet::new(),
            report: None,
            log: Vec::new(),
            scan: 0,
            pending_scan: false,
        }
    }

    /// 命中用户勾选的设备组、同厂商任意新接口，或（自动模式）键鼠类接口。
    pub fn in_scope(&self, info: &HidDeviceInfo) -> bool {
        let key = info.group_key();
        if self.selected.contains(&key) {
            return true;
        }
        let vid_prefix = format!("{:04X}:", info.vendor_id);
        if self.selected.iter().any(|k| k.starts_with(&vid_prefix)) {
            return true;
        }
        self.auto && info.is_kbms()
    }

    pub fn active_id(&self) -> Option<u8> {
        self.active.as_ref().map(|a| a.id)
    }
}

fn usage_tag(page: u16, usage: u16) -> Option<&'static str> {
    match (page, usage) {
        (0x0001, 0x0001 | 0x0002) => Some("鼠标"),
        (0x0001, 0x0006) => Some("键盘"),
        (0x0001, 0x0005) => Some("手柄"),
        (0x0001, 0x0008) => Some("多轴设备"),
        (0x000C, _) => Some("多媒体键"),
        (0x000D, _) => Some("触控/数位"),
        (0xFF00..=0xFFFF, _) => Some("厂商自定义"),
        _ => None,
    }
}

pub fn push_log(s: &mut Session, msg: &str) {
    let line = format!("{} {}", crate::sys::now_hms(), msg);
    crate::log::line(msg);
    s.log.push(line);
    if s.log.len() > 80 {
        s.log.remove(0);
    }
}

/// 扫描一次全部 HID 接口，更新设备组与在线快照。
pub fn scan(st: &Arc<Mutex<Session>>) {
    let paths = hid::list_paths();
    let mut s = match st.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    s.pending_scan = false;
    s.scan = s.scan.wrapping_add(1);

    for path in &paths {
        if s.cache.contains_key(path) {
            continue;
        }
        if let Some(info) = hid::inspect(path) {
            s.cache.insert(path.clone(), info);
        }
    }

    for g in s.groups.values_mut() {
        g.online = false;
        g.ifaces = 0;
    }

    let mut present: Vec<HidDeviceInfo> = Vec::new();
    for path in &paths {
        let info = match s.cache.get(path) {
            Some(i) => i.clone(),
            None => continue,
        };
        {
            let g = s
                .groups
                .entry(info.group_key())
                .or_insert_with(|| Group::new(&info));
            g.absorb(&info);
            g.online = true;
            g.ifaces += 1;
        }
        present.push(info);
    }
    s.devices = present;
}

/// 启动读取线程管理器：定时扫描 + 为作用域内新接口补线程。
pub fn spawn_manager(st: Arc<Mutex<Session>>, exit: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut last_scan = Instant::now() - Duration::from_secs(10);
        loop {
            if exit.load(Ordering::Relaxed) {
                return;
            }
            let need_scan = {
                match st.lock() {
                    Ok(s) => {
                        let interval = if s.active.is_some() { 1000 } else { 2500 };
                        s.pending_scan
                            || last_scan.elapsed() >= Duration::from_millis(interval)
                    }
                    Err(_) => return,
                }
            };
            if need_scan {
                scan(&st);
                last_scan = Instant::now();
            }
            ensure_readers(&st);
            std::thread::sleep(Duration::from_millis(300));
        }
    });
}

fn ensure_readers(st: &Arc<Mutex<Session>>) {
    let mut spawn: Vec<HidDeviceInfo> = Vec::new();
    {
        let mut s = match st.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if s.active.is_none() {
            return;
        }
        let candidates: Vec<HidDeviceInfo> = s
            .devices
            .iter()
            .filter(|i| s.in_scope(i))
            .cloned()
            .collect();
        for info in candidates {
            if s.running.contains(&info.path) {
                continue;
            }
            s.running.insert(info.path.clone());
            spawn.push(info);
        }
    }
    for info in spawn {
        spawn_reader(st.clone(), info);
    }
}

fn spawn_reader(st: Arc<Mutex<Session>>, info: HidDeviceInfo) {
    std::thread::spawn(move || {
        let len = if info.input_report_len > 0 {
            info.input_report_len
        } else {
            64
        }
        .clamp(8, 1024);
        let mut handle = match hid::open(&info.path) {
            Ok(h) => h,
            Err(_) => {
                drop_running(&st, &info.path);
                return;
            }
        };
        let mut buf = vec![0u8; len];
        let mut last: Vec<u8> = Vec::new();
        let mut last_rec = Instant::now() - Duration::from_secs(10);
        let mut reopen_fail = 0u32;
        loop {
            match handle.read_raw(&mut buf) {
                Ok(0) => {
                    // 0 字节返回：避免空转占满 CPU。
                    std::thread::sleep(Duration::from_millis(3));
                }
                Ok(n) => {
                    reopen_fail = 0;
                    let data = &buf[..n];
                    let changed = data != last.as_slice();
                    let now = Instant::now();
                    if changed || now.duration_since(last_rec) >= Duration::from_secs(3) {
                        last.clear();
                        last.extend_from_slice(data);
                        last_rec = now;
                        record(&st, &info.path, &info, data);
                    }
                }
                Err(_) => {
                    // 设备被拔掉/休眠：稍等再尝试重新打开，连续失败则退出线程。
                    std::thread::sleep(Duration::from_millis(300));
                    match hid::open(&info.path) {
                        Ok(h) => {
                            handle = h;
                            reopen_fail = 0;
                        }
                        Err(_) => {
                            reopen_fail += 1;
                            if reopen_fail >= 2 {
                                break;
                            }
                        }
                    }
                }
            }
        }
        drop_running(&st, &info.path);
    });
}

fn drop_running(st: &Arc<Mutex<Session>>, path: &str) {
    if let Ok(mut s) = st.lock() {
        s.running.remove(path);
    }
}

/// 收一帧：只在有活动步骤且命中作用域时落库。
pub fn record(st: &Arc<Mutex<Session>>, path: &str, info: &HidDeviceInfo, data: &[u8]) {
    let mut s = match st.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if !s.in_scope(info) {
        return;
    }
    let frame = if let Some(a) = s.active.as_mut() {
        if a.count >= MAX_PHASE_FRAMES {
            return;
        }
        let pc = a.path_counts.entry(path.to_string()).or_insert(0usize);
        if *pc >= MAX_PATH_FRAMES {
            return;
        }
        *pc += 1;
        a.count += 1;
        Some(Frame {
            phase: a.id,
            t_ms: a.started.elapsed().as_millis() as u64,
            path: path.to_string(),
            vid: info.vendor_id,
            pid: info.product_id,
            col: info.collection,
            usage_page: info.usage_page,
            usage: info.usage,
            data: data.to_vec(),
        })
    } else {
        None
    };
    if let Some(f) = frame {
        s.frames.push(f);
    }
}

/// 开始（或重做）某一步。会先结束正在进行的步骤，并清掉该步骤的旧数据。
pub fn start_phase(s: &mut Session, id: u8, plan: u32) {
    if !(2..=6).contains(&id) {
        push_log(s, &format!("忽略非法步骤 {id}"));
        return;
    }
    finish_active(s);
    s.frames.retain(|f| f.phase != id);
    s.phases.retain(|p| p.id != id);
    s.counts.remove(&id);
    let plan = plan.clamp(10, 1800);
    let name = step_name(id).to_string();
    s.active = Some(Active {
        id,
        name: name.clone(),
        started: Instant::now(),
        started_at: crate::sys::now_pretty(),
        plan,
        count: 0,
        path_counts: BTreeMap::new(),
    });
    s.pending_scan = true;
    push_log(
        s,
        &format!("开始第 {id} 步「{name}」，计划 {plan} 秒"),
    );
}

pub fn stop_phase(s: &mut Session, id: u8) {
    match &s.active {
        Some(a) if a.id == id => finish_active(s),
        Some(a) => {
            let cur = a.id;
            let name = a.name.clone();
            push_log(
                s,
                &format!("当前正在进行第 {cur} 步「{name}」，请先结束它"),
            );
        }
        None => push_log(s, "当前没有正在进行的步骤"),
    }
}

fn finish_active(s: &mut Session) {
    if let Some(a) = s.active.take() {
        let dur = a.started.elapsed().as_millis() as u64;
        s.counts.insert(a.id, a.count);
        s.phases.push(PhaseInfo {
            id: a.id,
            name: a.name.clone(),
            plan: a.plan,
            duration_ms: dur,
            frames: a.count,
            started_at: a.started_at.clone(),
        });
        push_log(
            s,
            &format!("结束第 {} 步「{}」，记录 {} 帧", a.id, a.name, a.count),
        );
    }
}

pub fn set_note(s: &mut Session, id: u8, text: &str) {
    let text: String = text.chars().take(2000).collect();
    if text.trim().is_empty() {
        s.notes.remove(&id);
    } else {
        s.notes.insert(id, text);
    }
}

// ---------------- 界面状态快照 ----------------

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

pub fn hex(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 3);
    for (i, b) in data.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}

pub fn snapshot_json(s: &Session) -> String {
    let mut out = String::with_capacity(4096);
    out.push('{');
    out.push_str(&format!("\"scan\":{},", s.scan));
    out.push_str(&format!("\"auto\":{},", s.auto));

    out.push_str("\"selected\":[");
    for (i, k) in s.selected.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{k}\""));
    }
    out.push_str("],");

    let mut groups: Vec<&Group> = s.groups.values().collect();
    groups.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then_with(|| a.display_name().cmp(&b.display_name()))
            .then_with(|| a.key.cmp(&b.key))
    });
    out.push_str("\"devices\":[");
    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"key\":\"{}\",\"vid\":\"{:04X}\",\"pid\":\"{:04X}\",\"name\":\"{}\",\"mfr\":\"{}\",\"ifaces\":{},\"online\":{},\"checked\":{},\"tags\":[",
            g.key,
            g.vid,
            g.pid,
            json_escape(&g.display_name()),
            json_escape(&g.manufacturer),
            g.ifaces,
            g.online,
            s.selected.contains(&g.key) || s.selected.iter().any(|k| k.starts_with(&format!("{:04X}:", g.vid)))
        ));
        for (j, t) in g.tags.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&format!("\"{}\"", json_escape(t)));
        }
        out.push_str("]}");
    }
    out.push_str("],");

    match &s.active {
        Some(a) => out.push_str(&format!(
            "\"phase\":{{\"id\":{},\"name\":\"{}\",\"plan\":{},\"elapsed\":{},\"frames\":{}}},",
            a.id,
            json_escape(&a.name),
            a.plan,
            a.started.elapsed().as_secs(),
            a.count
        )),
        None => out.push_str("\"phase\":null,"),
    }

    out.push_str("\"counts\":{");
    let mut first = true;
    for (id, n) in &s.counts {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!("\"{id}\":{n}"));
    }
    out.push_str("},");

    out.push_str("\"activeId\":");
    match s.active_id() {
        Some(id) => out.push_str(&id.to_string()),
        None => out.push_str("null"),
    }
    out.push(',');

    out.push_str("\"done\":[");
    let mut first = true;
    for id in s.counts.keys() {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&id.to_string());
    }
    out.push_str("],");

    out.push_str("\"notes\":{");
    let mut first = true;
    for (id, n) in &s.notes {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!("\"{id}\":\"{}\"", json_escape(n)));
    }
    out.push_str("},");

    // 最近 6 帧（只取当前步骤，避免串味）
    out.push_str("\"frames\":[");
    let tail: Vec<&Frame> = s
        .frames
        .iter()
        .rev()
        .filter(|f| Some(f.phase) == s.active_id())
        .take(6)
        .collect();
    for (i, f) in tail.iter().rev().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let col = f
            .col
            .map(|c| format!("COL{c:02}"))
            .unwrap_or_else(|| "COL??".into());
        out.push_str(&format!(
            "{{\"t\":{:.1},\"label\":\"{:04X}:{:04X}·{}\",\"hex\":\"{}\"}}",
            f.t_ms as f64 / 1000.0,
            f.vid,
            f.pid,
            col,
            hex(&f.data)
        ));
    }
    out.push_str("],");

    match &s.report {
        Some(r) => out.push_str(&format!(
            "\"report\":true,\"output\":\"{}\",\"files\":[{}],",
            json_escape(&r.dir),
            r.files
                .iter()
                .map(|f| format!("\"{}\"", json_escape(f)))
                .collect::<Vec<_>>()
                .join(",")
        )),
        None => out.push_str("\"report\":false,\"output\":\"\",\"files\":[],"),
    }

    out.push_str("\"log\":[");
    let tail_logs: Vec<&String> = s.log.iter().rev().take(6).collect();
    for (i, l) in tail_logs.iter().rev().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{}\"", json_escape(l)));
    }
    out.push_str("]}");
    out
}

/// 供 report.rs 使用的帧数（某步）。
#[allow(dead_code)]
pub fn phase_count(s: &Session, id: u8) -> usize {
    s.frames.iter().filter(|f| f.phase == id).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_rules() {
        let mut s = Session::new();
        let info = HidDeviceInfo {
            path: "x".into(),
            vendor_id: 0x24AE,
            product_id: 0x14A7,
            usage_page: 0x0001,
            usage: 0x0002,
            input_report_len: 19,
            collection: Some(9),
            product: "Mouse".into(),
            manufacturer: "Rapoo".into(),
        };
        assert!(s.in_scope(&info)); // auto 默认开启
        s.auto = false;
        assert!(!s.in_scope(&info));
        s.selected.insert("24AE:14A7".into());
        assert!(s.in_scope(&info));
        let mut other = info.clone();
        other.product_id = 0x46A7; // 同厂商的另一连接方式
        assert!(s.in_scope(&other));
        other.vendor_id = 0x046D;
        assert!(!s.in_scope(&other));
    }

    #[test]
    fn snapshot_is_valid_shape() {
        let mut s = Session::new();
        push_log(&mut s, "hi");
        let j = snapshot_json(&s);
        assert!(j.starts_with('{') && j.ends_with('}'));
        assert!(j.contains("\"phase\":null"));
    }
}

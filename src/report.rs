//! 抓包结果落盘与自动分析。
//!
//! 输出目录（exe 旁，不可写时退回 %LOCALAPPDATA%）：
//!   frames.jsonl        逐帧原始数据
//!   设备与字段分析.md    自动整理的字节分布、候选字段、跨步骤对比
//!   给AI的说明.md        给用户/AI 的使用说明
//!   设备列表.txt         本次涉及的 HID 接口清单

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::session::{self, Frame, Group, ReportOut, Session};

struct ByteStat {
    min: u8,
    max: u8,
    distinct: BTreeSet<u8>,
}

impl ByteStat {
    fn new() -> ByteStat {
        ByteStat {
            min: 255,
            max: 0,
            distinct: BTreeSet::new(),
        }
    }
    fn add(&mut self, v: u8) {
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.distinct.insert(v);
    }
    fn samples(&self) -> String {
        self.distinct
            .iter()
            .take(6)
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn path_label(f: &Frame) -> String {
    let col = f
        .col
        .map(|c| format!("COL{c:02}"))
        .unwrap_or_else(|| "COL??".into());
    format!(
        "VID_{:04X}&PID_{:04X}·{} (usage {:04X}/{:04X})",
        f.vid, f.pid, col, f.usage_page, f.usage
    )
}

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

fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}分{:02}秒", s / 60, s % 60)
}

fn guess(idx: usize, st: &ByteStat) -> &'static str {
    if idx == 0 {
        return "Report ID";
    }
    let d = st.distinct.len();
    if st.max <= 100 && st.min >= 1 && d >= 2 {
        return "电量候选";
    }
    if st.max <= 4 && d >= 2 && d <= 4 {
        return "状态位候选";
    }
    if d == 1 {
        return "常量";
    }
    ""
}

/// 生成报告。会先把正在进行的步骤收尾，保证数据完整。
pub fn generate(s: &mut Session) -> Result<ReportOut, String> {
    if s.active.is_some() {
        if let Some(id) = s.active_id() {
            session::stop_phase(s, id);
        }
    }
    if s.frames.is_empty() {
        return Err("还没有抓到任何数据：请先在第 2~6 步点【开始抓包】".into());
    }

    let stamp = crate::sys::now_stamp();
    let root = crate::sys::output_root();
    let mut dir: PathBuf = root.join(format!("外设抓包结果_{stamp}"));
    if dir.exists() {
        dir = root.join(format!("外设抓包结果_{stamp}_2"));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建结果目录失败：{e}"))?;

    let frames = s.frames.clone();
    let phases = &s.phases;
    let notes = &s.notes;
    let counts = &s.counts;
    let groups = &s.groups;
    let selected = &s.selected;
    let auto = s.auto;

    // 1) frames.jsonl
    {
        let mut body = String::with_capacity(frames.len() * 120);
        for f in &frames {
            let col = f.col.map(|c| c.to_string()).unwrap_or_else(|| "null".into());
            let _ = writeln!(
                body,
                "{{\"phase\":{},\"step\":\"{}\",\"t\":{:.3},\"path\":\"{}\",\"vid\":\"{:04X}\",\"pid\":\"{:04X}\",\"col\":{},\"usage_page\":\"{:04X}\",\"usage\":\"{:04X}\",\"hex\":\"{}\"}}",
                f.phase,
                session::step_name(f.phase),
                f.t_ms as f64 / 1000.0,
                json_escape(&f.path),
                f.vid,
                f.pid,
                col,
                f.usage_page,
                f.usage,
                session::hex(&f.data)
            );
        }
        std::fs::write(dir.join("frames.jsonl"), body)
            .map_err(|e| format!("写入 frames.jsonl 失败：{e}"))?;
    }

    // 2) 设备与字段分析.md
    let analysis = build_analysis(&frames, phases, counts, notes, groups, selected, auto);
    std::fs::write(dir.join("设备与字段分析.md"), analysis)
        .map_err(|e| format!("写入分析报告失败：{e}"))?;

    // 3) 给AI的说明.md
    std::fs::write(
        dir.join("给AI的说明.md"),
        ai_readme(&stamp, frames.len(), phases.len()),
    )
    .map_err(|e| format!("写入说明失败：{e}"))?;

    // 4) 设备列表.txt
    let mut dev = String::new();
    let _ = writeln!(dev, "外设抓包工具 · 设备列表（{}）", crate::sys::now_pretty());
    let _ = writeln!(dev, "说明：同一物理设备可能有多个 HID 接口（COL01/COL02/...），属正常现象。\n");
    for g in groups.values() {
        let _ = writeln!(
            dev,
            "{}\n  VID:PID = {:04X}:{:04X}\n  厂商 = {}\n  类型 = {}\n  接口数 = {}\n  本次勾选 = {}\n",
            g.display_name(),
            g.vid,
            g.pid,
            if g.manufacturer.is_empty() { "未知" } else { &g.manufacturer },
            if g.tags.is_empty() {
                "未识别".to_string()
            } else {
                g.tags.iter().cloned().collect::<Vec<_>>().join(" / ")
            },
            g.ifaces,
            if selected.contains(&g.key) { "是" } else { "否" }
        );
    }
    std::fs::write(dir.join("设备列表.txt"), dev)
        .map_err(|e| format!("写入设备列表失败：{e}"))?;

    let dir_str = dir.to_string_lossy().to_string();
    let files = vec![
        "设备与字段分析.md".to_string(),
        "给AI的说明.md".to_string(),
        "frames.jsonl".to_string(),
        "设备列表.txt".to_string(),
    ];
    session::push_log(s, &format!("报告已生成：{dir_str}"));
    Ok(ReportOut {
        dir: dir_str,
        files,
    })
}

fn build_analysis(
    frames: &[Frame],
    phases: &[session::PhaseInfo],
    counts: &BTreeMap<u8, usize>,
    notes: &BTreeMap<u8, String>,
    groups: &BTreeMap<String, Group>,
    selected: &BTreeSet<String>,
    auto: bool,
) -> String {
    let mut md = String::with_capacity(16 * 1024);
    let _ = writeln!(md, "# 外设抓包结果 · 自动分析\n");
    let _ = writeln!(md, "- 采集时间：{}", crate::sys::now_pretty());
    let _ = writeln!(md, "- 工具：外设抓包工具 1.0.0");
    let _ = writeln!(md, "- 总帧数：{}", frames.len());
    let _ = writeln!(
        md,
        "- 抓取范围：{} + 勾选 {} 个设备组\n",
        if auto { "自动（键鼠类 HID）" } else { "仅手动勾选" },
        selected.len()
    );

    // 一、设备
    let _ = writeln!(md, "## 一、本次涉及的设备\n");
    let _ = writeln!(md, "| 设备 | VID:PID | 厂商 | 类型 | 接口数 | 勾选 |");
    let _ = writeln!(md, "| --- | --- | --- | --- | --- | --- |");
    for g in groups.values() {
        let tags = if g.tags.is_empty() {
            "未识别".to_string()
        } else {
            g.tags.iter().cloned().collect::<Vec<_>>().join(" / ")
        };
        let _ = writeln!(
            md,
            "| {} | {:04X}:{:04X} | {} | {} | {} | {} |",
            g.display_name(),
            g.vid,
            g.pid,
            if g.manufacturer.is_empty() {
                "-"
            } else {
                &g.manufacturer
            },
            tags,
            g.ifaces,
            if selected.contains(&g.key) { "是" } else { "否" }
        );
    }
    let _ = writeln!(md);

    // 二、步骤概况
    let _ = writeln!(md, "## 二、步骤概况\n");
    let _ = writeln!(md, "| 步骤 | 开始时间 | 计划时长 | 实际时长 | 帧数 | 备注 |");
    let _ = writeln!(md, "| --- | --- | --- | --- | --- | --- |");
    for p in phases {
        let note = notes.get(&p.id).cloned().unwrap_or_default();
        let _ = writeln!(
            md,
            "| {} · {} | {} | {} 秒 | {} | {} | {} |",
            p.id,
            p.name,
            p.started_at,
            p.plan,
            fmt_ms(p.duration_ms),
            p.frames,
            note.replace('\n', "<br>")
        );
    }
    let _ = writeln!(md);

    // 三、按步骤/接口的字节分布
    let _ = writeln!(md, "## 三、各接口字节分布\n");
    let _ = writeln!(
        md,
        "> 读表提示：HID 输入报告的第 0 个字节通常是 Report ID；电量一般出现在 1..=100 的字节里；\n> 0/1/2/3 这类小值多为状态位（充电、连接、灯光等）。\n"
    );

    let mut by_step: BTreeMap<(u8, String), Vec<&Frame>> = BTreeMap::new();
    for f in frames {
        by_step
            .entry((f.phase, f.path.clone()))
            .or_default()
            .push(f);
    }

    let mut current_step = 0u8;
    for ((phase, _path), list) in &by_step {
        if *phase != current_step {
            current_step = *phase;
            let _ = writeln!(
                md,
                "\n### 步骤 {} · {}（{} 帧）\n",
                phase,
                session::step_name(*phase),
                counts.get(phase).copied().unwrap_or(list.len())
            );
        }
        let first = list[0];
        let label = path_label(first);
        let _ = writeln!(md, "#### {label}\n");
        let _ = writeln!(md, "- 帧数：{}", list.len());
        if list.len() >= 2 {
            let mut gaps: Vec<u64> = list
                .windows(2)
                .map(|w| w[1].t_ms.saturating_sub(w[0].t_ms))
                .collect();
            gaps.sort_unstable();
            let avg = gaps.iter().sum::<u64>() / gaps.len() as u64;
            let _ = writeln!(
                md,
                "- 帧间隔：最小 {} ms / 中位 {} ms / 平均 {} ms / 最大 {} ms（越大越节约电）",
                gaps[0],
                gaps[gaps.len() / 2],
                avg,
                gaps[gaps.len() - 1]
            );
        }
        let _ = writeln!(md);

        let max_len = list.iter().map(|f| f.data.len()).max().unwrap_or(0);
        let mut stats: Vec<ByteStat> = (0..max_len).map(|_| ByteStat::new()).collect();
        for f in list {
            for (i, b) in f.data.iter().enumerate() {
                stats[i].add(*b);
            }
        }
        let _ = writeln!(md, "| 字节 | 最小 | 最大 | 不同取值数 | 样例 | 猜测 |");
        let _ = writeln!(md, "| --- | --- | --- | --- | --- | --- |");
        for (i, st) in stats.iter().enumerate() {
            let _ = writeln!(
                md,
                "| b{} | {} | {} | {} | {} | {} |",
                i,
                st.min,
                st.max,
                st.distinct.len(),
                st.samples(),
                guess(i, st)
            );
        }
        let _ = writeln!(md);
    }

    // 四、跨步骤对比（同一 VID:PID 出现在多个步骤时，找出不同字节）
    let _ = writeln!(md, "## 四、跨步骤字节对比\n");
    let _ = writeln!(
        md,
        "> 同一个设备型号在不同连接方式（2.4G / 有线 / 蓝牙 / 充电）下，变化的字节通常是连接状态或充电状态位。\n"
    );
    let mut by_dev: BTreeMap<(u16, u16), BTreeMap<u8, Vec<&Frame>>> = BTreeMap::new();
    for f in frames {
        by_dev
            .entry((f.vid, f.pid))
            .or_default()
            .entry(f.phase)
            .or_default()
            .push(f);
    }
    let mut any_diff = false;
    for ((vid, pid), phases_map) in &by_dev {
        if phases_map.len() < 2 {
            continue;
        }
        let _ = writeln!(md, "### VID_{vid:04X}&PID_{pid:04X}\n");
        let max_len = phases_map
            .values()
            .flat_map(|v| v.iter().map(|f| f.data.len()))
            .max()
            .unwrap_or(0);
        let mut printed = 0;
        for i in 0..max_len {
            let mut per_phase: Vec<(u8, BTreeSet<u8>)> = Vec::new();
            for (phase, list) in phases_map {
                if list.len() < 3 {
                    continue;
                }
                let mut set = BTreeSet::new();
                for f in list {
                    if let Some(v) = f.data.get(i) {
                        set.insert(*v);
                    }
                }
                if !set.is_empty() {
                    per_phase.push((*phase, set));
                }
            }
            if per_phase.len() < 2 {
                continue;
            }
            let all_same = per_phase
                .windows(2)
                .all(|w| w[0].1 == w[1].1);
            if all_same {
                continue;
            }
            any_diff = true;
            printed += 1;
            let desc = per_phase
                .iter()
                .map(|(p, set)| {
                    format!(
                        "步骤{p}=[{}]",
                        set.iter()
                            .take(5)
                            .map(|v| v.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                })
                .collect::<Vec<_>>()
                .join("  ");
            let _ = writeln!(md, "- b{i}: {desc}");
            if printed >= 40 {
                let _ = writeln!(md, "- ……（其余字节省略）");
                break;
            }
        }
        if printed == 0 {
            let _ = writeln!(md, "- 未发现跨步骤差异（可能各连接方式走了不同接口）。");
        }
        let _ = writeln!(md);
    }
    if !any_diff {
        let _ = writeln!(md, "（没有可用于对比的多步骤同型号数据。）\n");
    }

    // 五、关键帧样例
    let _ = writeln!(md, "## 五、关键帧样例\n");
    let _ = writeln!(
        md,
        "> 已按内容去重，每接口最多 10 条，便于快速看出帧结构。\n"
    );
    for ((phase, _path), list) in &by_step {
        let first = list[0];
        let _ = writeln!(
            md,
            "### 步骤 {} · {}\n",
            phase,
            path_label(first)
        );
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut shown = 0;
        for f in list {
            let h = session::hex(&f.data);
            if !seen.insert(h.clone()) {
                continue;
            }
            let _ = writeln!(md, "- t={:.1}s  `{h}`", f.t_ms as f64 / 1000.0);
            shown += 1;
            if shown >= 10 {
                break;
            }
        }
        let _ = writeln!(md);
    }

    // 六、给 AI 的提示
    let _ = writeln!(md, "## 六、结论与下一步\n");
    let mut cands: Vec<String> = Vec::new();
    let mut consts: Vec<String> = Vec::new();
    for ((phase, _path), list) in &by_step {
        let max_len = list.iter().map(|f| f.data.len()).max().unwrap_or(0);
        let mut stats: Vec<ByteStat> = (0..max_len).map(|_| ByteStat::new()).collect();
        for f in list {
            for (i, b) in f.data.iter().enumerate() {
                stats[i].add(*b);
            }
        }
        for (i, st) in stats.iter().enumerate() {
            let short = path_label(list[0])
                .split(' ')
                .next()
                .unwrap_or("")
                .to_string();
            if guess(i, st) == "电量候选" {
                cands.push(format!(
                    "- 步骤{phase} · {short} · b{i}：取值 {}~{}（{} 种）",
                    st.min,
                    st.max,
                    st.distinct.len()
                ));
            } else if i > 1 && st.distinct.len() == 1 && st.max >= 1 && st.max <= 100 {
                consts.push(format!(
                    "- 步骤{phase} · {short} · b{i} = {}（本步未变化）",
                    st.max
                ));
            }
        }
    }
    if cands.is_empty() {
        let _ = writeln!(
            md,
            "- 本次没有自动锁定“有变化且落在 1..=100”的电量候选，建议把 `frames.jsonl` 一并交给 AI 分析。"
        );
    } else {
        let _ = writeln!(md, "自动找到的电量候选（本步内有变化）：\n");
        for c in cands.iter().take(20) {
            let _ = writeln!(md, "{c}");
        }
    }
    if !consts.is_empty() {
        let _ = writeln!(
            md,
            "\n数值落在 1..100 但本步没变化的字节（需要靠跨步骤对比或充电前后对比来确认）：\n"
        );
        for c in consts.iter().take(12) {
            let _ = writeln!(md, "{c}");
        }
    }
    let _ = writeln!(
        md,
        "\n建议把整个结果目录压缩后发给 AI，并附上设备型号与各步骤的备注（充电灯颜色、软件显示电量等）。"
    );
    md
}

fn ai_readme(stamp: &str, frame_count: usize, phase_count: usize) -> String {
    format!(
        r#"# 给 AI 的说明（请把整个文件夹一起发送）

这是一个外设（键鼠）的 HID 原始抓包结果，采集于 {stamp}，共 {frame_count} 帧、{phase_count} 个步骤。

## 文件说明

| 文件 | 内容 |
| --- | --- |
| `frames.jsonl` | 逐帧原始数据，每行一个 JSON。`hex` 是完整报告（含 Report ID），`t` 是该步骤内的秒数，`phase` 是步骤编号。 |
| `设备与字段分析.md` | 自动整理的字节分布、候选电量字节、跨步骤差异字节、关键帧样例。 |
| `设备列表.txt` | 本次抓到的 HID 接口清单（VID/PID/类型）。 |
| `给AI的说明.md` | 本文件。 |

## 步骤编号

2 = 2.4G 接收器；3 = 有线 USB；4 = 蓝牙；5 = 充电状态；6 = 电量变化。

## 建议向 AI 提出的问题

1. 这个设备的电量字段在哪个字节？编码方式是百分比还是分级（如 0~5）？
2. 充电状态字段是哪个字节？取值分别代表什么（放电 / 充电中 / 充满）？
3. 2.4G、有线、蓝牙三种模式的上报格式是否一致？不一致的话差异在哪里？
4. 请给我一段可运行的解析代码（语言不限），输入一帧 hex，输出电量和充电状态。

## 注意事项

- 同一物理设备会有多个 HID 接口（COL01/COL02/...），只有其中一个包含电量数据。
- 第 0 个字节通常是 Report ID；`frames.jsonl` 里的 `hex` 已包含它。
- 若某些步骤帧数很少，说明设备在该模式下没有主动上报（多数设备休眠时会停发）。
"#
    )
}

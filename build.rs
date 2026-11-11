use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");

    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }

    match find_rc() {
        Some(rc) => {
            let res = PathBuf::from(env::var("OUT_DIR").unwrap()).join("app.res");
            let ok = Command::new(&rc)
                .arg("/nologo")
                .arg("/fo")
                .arg(&res)
                .arg("app.rc")
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                println!("cargo:rustc-link-arg={}", res.display());
            } else {
                println!("cargo:warning=rc.exe 编译图标失败，exe 将无图标");
            }
        }
        None => println!("cargo:warning=未找到 rc.exe，exe 将无图标"),
    }
}

/// 在 Windows SDK 目录里找最新版本的 x64 rc.exe。
fn find_rc() -> Option<PathBuf> {
    let root = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut hits: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path().join("x64").join("rc.exe"))
        .filter(|p| p.exists())
        .collect();
    hits.sort();
    hits.pop()
}

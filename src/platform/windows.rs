//! Windows 平台实现。
//!
//! 打开 / 选中走 `ShellExecute` 语义（`open` crate）与资源管理器命令行；剪贴板
//! 走 `arboard`；删目录走 `trash` crate 的 `IFileOperation`（**带回滚站**，不是
//! 直接 `RemoveDirectory`）。
//!
//! 这里刻意不用原始 `windows` crate 调用去实现这四件事：`open` / `trash` /
//! `arboard` 已经把「路径编码、COM 初始化、双 NUL 结尾」这些坑封好了，
//! 而我们没有 Windows 机器做逐行调试。`windows` crate 留给真正没有现成封装的
//! 能力（窗口置顶、光标所在显示器、前台激活）。

use std::path::{Path, PathBuf};

pub(super) fn open_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("路径不存在：{}", path.display()));
    }
    // detached：不要把调用线程卡在目标进程的生命周期上（打开一个 GUI 程序
    // 可能持续数小时）。工作线程本来就等得起，但没必要占着。
    open::that_detached(path).map_err(|e| format!("打开 {} 失败：{e}", path.display()))
}

pub(super) fn reveal_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("路径不存在：{}", path.display()));
    }
    // explorer 的退出码不表示成功（常见 1），所以只看「能不能起进程」。
    std::process::Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("调用资源管理器失败：{e}"))
}

pub(super) fn copy_text(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("访问剪贴板失败：{e}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| format!("写入剪贴板失败：{e}"))
}

pub(super) fn trash_path(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| format!("移到回收站失败：{e}"))
}

/// 回收站不是一个能直接 `rename` 进去的普通目录（里面是 `$R*` / `$I*` 元数据
/// 对），所以不提供纯 fs 回退——失败就让调用方报错并保留目录。
pub(super) fn trash_dir() -> Option<PathBuf> {
    None
}

/// Windows 屏幕坐标本来就是左上原点、y 向下，与入参同一套，无需换算。
pub(super) fn window_top_left(x: f64, y: f64) -> (f64, f64) {
    (x, y)
}

/// 开始菜单的两个 `Programs` 目录：当前用户 + 所有用户。第三方安装器往这里
/// 放 `.lnk`，扫它们就等于扫「已安装的应用」。
pub(super) fn app_roots() -> Vec<(PathBuf, &'static str)> {
    let mut roots = Vec::new();
    if let Ok(program_data) = std::env::var("ProgramData") {
        roots.push((
            PathBuf::from(program_data).join(r"Microsoft\Windows\Start Menu\Programs"),
            "所有用户",
        ));
    }
    if let Ok(app_data) = std::env::var("APPDATA") {
        roots.push((
            PathBuf::from(app_data).join(r"Microsoft\Windows\Start Menu\Programs"),
            "当前用户",
        ));
    }
    roots
}

pub(super) fn shell_command(cmd: &str) -> (String, Vec<String>) {
    (
        "cmd.exe".to_string(),
        vec!["/C".to_string(), cmd.to_string()],
    )
}

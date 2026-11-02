//! 平台边界。
//!
//! 宿主代码（模块、视图、动作）只调这里的函数，不直接写 `#[cfg(target_os)]`、
//! 也不直接拼 `/usr/bin/open`、`explorer /select,` 这类平台命令。这样
//! 「换一个平台」的成本集中在本目录，而不是散落在几十个业务文件里。
//!
//! 分层：
//!
//! - 本文件放**与平台无关**的部分：工作线程调度、废纸篓的文件系统回退；
//! - `macos.rs` / `windows.rs` 各自实现同一组 `imp::*` 函数；
//! - 两边都不支持的平台退到 `unsupported.rs`，返回明确错误，不假装成功。
//!
//! 加一个新平台 = 新建一个文件实现那 5 个 `imp::*`，再在下面补两行 `mod`。

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as imp;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use unsupported as imp;

/// 复制纯文本到系统剪贴板。
pub(crate) fn copy_text(text: &str) -> Result<(), String> {
    imp::copy_text(text)
}

/// 在工作线程里用默认程序**打开**路径（目录就是开一个文件管理器窗口）。
///
/// 结果回调也在工作线程上执行，里面只能做线程安全的事：写 `ConsoleChannel`、
/// 推通知卡片都可以，**别碰 `App`**。
pub(crate) fn open_in_background(
    path: PathBuf,
    done: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    off_thread(move || imp::open_path(&path), done);
}

/// 在工作线程里在文件管理器中**选中**路径（定位到它所在的那一层）。
pub(crate) fn reveal_in_background(
    path: PathBuf,
    done: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    off_thread(move || imp::reveal_path(&path), done);
}

/// 把路径移进系统废纸篓 / 回收站。失败时**不**删，由调用方决定回退或报错。
pub(crate) fn trash_item(path: &Path) -> Result<(), String> {
    imp::trash_path(path)
}

/// 系统废纸篓目录；平台没有可写的"普通目录"式废纸篓时返回 `None`，
/// 调用方就只剩 [`trash_item`] 一条路。
pub(crate) fn trash_dir() -> Option<PathBuf> {
    imp::trash_dir()
}

/// 把「屏幕坐标里的顶左点」换算成该平台开窗定位需要的形式。
///
/// 入参是主屏左上角为原点、y 向下的坐标（截屏 / 光标查询用的就是这套）。
/// macOS 的 AppKit 全局坐标以主屏**左下角**为原点、y 向上，要翻一次；
/// Windows 本来就是左上原点，原样返回。
pub(crate) fn window_top_left(x: f64, y: f64) -> (f64, f64) {
    imp::window_top_left(x, y)
}

/// 应用扫描根目录，附一个给人看的说明（进 subtitle）。
pub(crate) fn app_roots() -> Vec<(PathBuf, &'static str)> {
    imp::app_roots()
}

/// 执行一段 shell 命令的程序与参数：POSIX 是 `sh -c`，Windows 是 `cmd /C`。
pub(crate) fn shell_command(cmd: &str) -> (String, Vec<String>) {
    imp::shell_command(cmd)
}

/// 纯文件系统回退：把 `path` 挪进 `trash_dir`，重名时加 `-<unix 秒>` 后缀。
///
/// 与 [`trash_item`] 分开是为了可测：不依赖任何平台 API，也不碰真实废纸篓。
pub(crate) fn move_to_trash(path: &Path, trash_dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(trash_dir)
        .map_err(|e| format!("创建 {} 失败：{e}", trash_dir.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("路径没有文件名：{}", path.display()))?;
    let mut target = trash_dir.join(name);
    if target.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();
        let mut candidate = std::ffi::OsString::from(name);
        candidate.push(format!("-{stamp}"));
        target = trash_dir.join(candidate);
        let mut index = 1;
        while target.exists() {
            let mut candidate = std::ffi::OsString::from(name);
            candidate.push(format!("-{stamp}-{index}"));
            target = trash_dir.join(candidate);
            index += 1;
        }
    }
    std::fs::rename(path, &target).map_err(|e| format!("移动到 {} 失败：{e}", target.display()))?;
    Ok(target)
}

/// 起一条工作线程执行 `work`，结果交给 `done`。
///
/// **GPUI 回调里（点击处理器、`LauncherAction::run`）一律走 [`open_in_background`]
/// / [`reveal_in_background`]**，别直接 `std::process::Command`：
///
/// - 同步调用会卡住 UI 帧，直到子进程退出；
/// - 更要紧的是激活别的 App 这件事。曾经这里直接调
///   `-[NSWorkspace selectFile:inFileViewerRootedAtPath:]`，它在**当前线程同步
///   激活 Finder**；点按钮那一刻我们正处在 GPUI 的窗口事件更新里（App 已被
///   借用），AppKit 紧接着把 `windowDidResignKey` / `applicationDidResignActive`
///   同步派发回来，GPUI 再借一次 App → `RefCell already borrowed` 直接 abort。
///   工作线程里既没有 App 借用，也不占 UI 帧，激活回调落到哪都无所谓。
fn off_thread(
    work: impl FnOnce() -> Result<(), String> + Send + 'static,
    done: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    std::thread::spawn(move || done(work()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn move_to_trash_moves_and_suffixes_on_conflict() {
        let tmp = TempDir::new("platform-trash");
        let trash = tmp.0.join("Trash");
        let extension = tmp.0.join("demo");
        std::fs::create_dir_all(&extension).unwrap();
        std::fs::write(extension.join("extension.json"), "{}").unwrap();

        let target = move_to_trash(&extension, &trash).expect("first move");
        assert!(!extension.exists());
        assert!(target.join("extension.json").is_file());

        // 同名再进来一次：不能覆盖废纸篓里已有的，要加后缀。
        std::fs::create_dir_all(&extension).unwrap();
        let second = move_to_trash(&extension, &trash).expect("second move");
        assert_ne!(second, target);
        assert!(second.is_dir());
        assert!(target.join("extension.json").is_file());
    }

    #[test]
    fn move_to_trash_reports_a_missing_source() {
        let tmp = TempDir::new("platform-trash-missing");
        let error =
            move_to_trash(&tmp.0.join("nope"), &tmp.0.join("Trash")).expect_err("must fail");
        assert!(!error.is_empty());
    }
}

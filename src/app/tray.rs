//! macOS 菜单栏托盘（NSStatusItem）。
//!
//! gpui-kit 的 `StatusBar` 只是窗口内的 UI 组件，不提供 NSStatusItem 能力，
//! 这里直接用 objc2 raw `msg_send!` 实现（与 `ui/mac_ffi.rs` 同一套约束：
//! 不引入 objc2-app-kit / objc2-foundation，避免 objc2 0.5/0.6 双版本冲突）。
//!
//! 结构：
//! - `TrayCommand`：菜单项点击后经 crossbeam channel 发往 app tick 执行的命令
//!   （ObjC 回调里不能碰 gpui 的 `App`，必须走 channel 回到主循环，与
//!   `launcher_tx` 同一模式）。
//! - `init(tx)`：在主线程创建 NSStatusItem + NSMenu，全部对象由系统持有 /
//!   故意泄漏（target 是 weak 引用，handler 必须活满进程生命周期）。
//! - 图标：18×18 闪电符号，用 NSBezierPath 手绘 + `setTemplate:YES`，
//!   自动适配菜单栏浅色/深色/壁纸着色，无需准备多份 PNG。

use std::sync::OnceLock;

use crossbeam_channel::{Receiver, Sender};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{class, msg_send};

/// 托盘菜单点击后要执行的命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayCommand {
    ToggleLauncher,
    OpenManager,
    NewScript,
    Quit,
}

/// ObjC 回调 → Rust 主循环的命令通道。`init` 时建立，app tick 里 drain。
static TRAY_TX: OnceLock<Sender<TrayCommand>> = OnceLock::new();

/// 建立命令通道的接收端。必须在 `init` 之前调用一次。
pub(crate) fn command_channel() -> Receiver<TrayCommand> {
    let (tx, rx) = crossbeam_channel::unbounded();
    // 重复调用不覆盖已有 sender（应用只有一个托盘）。
    let _ = TRAY_TX.set(tx);
    rx
}

fn post(cmd: TrayCommand) {
    if let Some(tx) = TRAY_TX.get() {
        let _ = tx.send(cmd);
    }
}

/// 在主线程创建托盘图标与菜单。应在 gpui `.run()` 回调内调用（此时 NSApp
/// 已就绪）。
#[cfg(target_os = "macos")]
pub(crate) fn init() {
    // handler 类只注册一次。
    let handler_class = ensure_handler_class();
    let Some(handler_class) = handler_class else {
        eprintln!("[tray] 注册 handler 类失败，托盘不可用");
        return;
    };

    unsafe {
        // handler 实例必须活满整个进程：NSMenuItem 的 target 是 weak。
        let handler: *mut AnyObject = msg_send![handler_class, new];

        // NSStatusBar.systemStatusBar → statusItemWithLength:NSVariable (-1.0)
        let status_bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
        let item: *mut AnyObject = msg_send![status_bar, statusItemWithLength: -1.0_f64];
        if item.is_null() {
            eprintln!("[tray] 创建 NSStatusItem 失败");
            return;
        }
        // `statusItemWithLength:` 返回 autoreleased 对象；gpui run 回调所在的
        // autorelease pool 排空后 item 被释放 → 托盘图标出现一下又消失（或
        // 根本不出现）。显式 retain +1，活到进程退出（常驻进程不释放）。
        let _: *mut AnyObject = msg_send![item, retain];

        // 图标：手绘闪电，模板图自动适配菜单栏主题。
        let button: *mut AnyObject = msg_send![item, button];
        if !button.is_null() {
            let image = make_bolt_image();
            if !image.is_null() {
                let _: () = msg_send![button, setImage: image];
            }
            let tooltip = crate::ui::mac_ffi::ns_string("Scripter — Alt+Space 唤起");
            if !tooltip.is_null() {
                let _: () = msg_send![button, setToolTip: tooltip];
            }
        }

        // 菜单。
        let menu: *mut AnyObject = msg_send![class!(NSMenu), new];
        add_menu_item(menu, handler, "打开启动器", "showLauncher:", None);
        add_menu_item(menu, handler, "脚本管理", "openManager:", None);
        add_menu_item(menu, handler, "新建脚本", "newScript:", None);
        let sep: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep];
        add_menu_item(
            menu,
            handler,
            "退出 Scripter",
            "quitApp:",
            Some(("q", 0x10_0000)),
        ); // ⌘Q

        let _: () = msg_send![item, setMenu: menu];

        // handler 的 +1 引用来自 `+[new]`，永不 send release 即天然泄漏——
        // NSMenuItem 的 target 是 weak，这个对象必须活满进程生命周期。
        // （raw pointer 是 Copy，mem::forget 无意义，不写 release 就够了。）
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn init() {}

/// 追加一个菜单项。`key` 是 (显示字符, NSEventModifierFlags)。
#[cfg(target_os = "macos")]
unsafe fn add_menu_item(
    menu: *mut AnyObject,
    target: *mut AnyObject,
    title: &str,
    action: &str,
    key: Option<(&str, u64)>,
) {
    use std::ffi::CString;

    let title_ns = crate::ui::mac_ffi::ns_string(title);
    let key_ns = crate::ui::mac_ffi::ns_string(key.map(|(k, _)| k).unwrap_or(""));
    let sel = CString::new(action).unwrap();
    // sel! 宏只接受字面量，action 名是参数，用运行时注册（0.6 起是 safe fn）。
    let sel = objc2::runtime::Sel::register(sel.as_c_str());
    let item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
    let item: *mut AnyObject =
        msg_send![item, initWithTitle: title_ns, action: sel, keyEquivalent: key_ns];
    let _: () = msg_send![item, setTarget: target];
    if let Some((_, mask)) = key {
        let _: () = msg_send![item, setKeyEquivalentModifierMask: mask as usize];
    }
    let _: () = msg_send![menu, addItem: item];
}

/// 注册 `ScripterTrayHandler` ObjC 类，四个方法各自 post 一条 TrayCommand。
#[cfg(target_os = "macos")]
fn ensure_handler_class() -> Option<&'static AnyClass> {
    use objc2::declare::ClassBuilder;
    use objc2::runtime::Sel;

    const NAME: &std::ffi::CStr = c"ScripterTrayHandler";
    if let Some(cls) = AnyClass::get(NAME) {
        return Some(cls);
    }

    let superclass = AnyClass::get(c"NSObject")?;
    let mut builder = ClassBuilder::new(NAME, superclass)?;

    unsafe extern "C-unwind" fn show_launcher(
        _this: &AnyObject,
        _cmd: Sel,
        _sender: *mut AnyObject,
    ) {
        post(TrayCommand::ToggleLauncher);
    }
    unsafe extern "C-unwind" fn open_manager(
        _this: &AnyObject,
        _cmd: Sel,
        _sender: *mut AnyObject,
    ) {
        post(TrayCommand::OpenManager);
    }
    unsafe extern "C-unwind" fn new_script(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
        post(TrayCommand::NewScript);
    }
    unsafe extern "C-unwind" fn quit_app(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
        post(TrayCommand::Quit);
    }

    unsafe {
        // 官方写法（objc2 0.6 define.rs 文档示例）：`fn(_, _, _)` 占位让编译器
        // 推导 HRTB 生命周期；写全类型反而触发 "not general enough"。
        builder.add_method(
            objc2::sel!(showLauncher:),
            show_launcher as unsafe extern "C-unwind" fn(_, _, _),
        );
        builder.add_method(
            objc2::sel!(openManager:),
            open_manager as unsafe extern "C-unwind" fn(_, _, _),
        );
        builder.add_method(
            objc2::sel!(newScript:),
            new_script as unsafe extern "C-unwind" fn(_, _, _),
        );
        builder.add_method(
            objc2::sel!(quitApp:),
            quit_app as unsafe extern "C-unwind" fn(_, _, _),
        );
    }

    Some(builder.register())
}

/// 画一个 18×18 的闪电符号（黑色填充 + setTemplate:YES，系统按菜单栏主题
/// 重新着色）。坐标系是 AppKit 默认的左下原点。
#[cfg(target_os = "macos")]
fn make_bolt_image() -> *mut AnyObject {
    // core-graphics 的 CGSize/CGPoint 没实现 objc2 的 `Encode`（msg_send!
    // by-value 传参需要），用同布局的本地 repr(C) 结构体 + 手写 Encode。
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSPoint {
        x: f64,
        y: f64,
    }
    unsafe impl objc2::Encode for NSSize {
        const ENCODING: objc2::Encoding =
            objc2::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl objc2::Encode for NSPoint {
        const ENCODING: objc2::Encoding =
            objc2::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }

    unsafe {
        let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let image: *mut AnyObject = msg_send![image, initWithSize: NSSize {
            width: 18.0,
            height: 18.0,
        }];
        if image.is_null() {
            return image;
        }

        let _: () = msg_send![image, lockFocusFlipped: false];

        let path: *mut AnyObject = msg_send![class!(NSBezierPath), bezierPath];
        // 闪电多边形（18×18，左下原点）：
        //   从顶部偏右起笔 → 折到中部偏左 → 内凹 → 底部偏左 → 折回中部偏右 → 内凹闭合。
        let pts = [
            (10.4, 17.2),
            (4.6, 9.6),
            (8.4, 9.6),
            (7.4, 0.8),
            (13.4, 8.4),
            (9.4, 8.4),
        ];
        let _: () = msg_send![path, moveToPoint: NSPoint { x: pts[0].0, y: pts[0].1 }];
        for p in &pts[1..] {
            let _: () = msg_send![path, lineToPoint: NSPoint { x: p.0, y: p.1 }];
        }
        let _: () = msg_send![path, closePath];

        // 圆角连接，视觉更柔和。
        let _: () = msg_send![path, setLineJoinStyle: 1_usize]; // NSLineJoinStyleRound
        let black: *mut AnyObject = msg_send![class!(NSColor), blackColor];
        let _: () = msg_send![black, setFill];
        let _: () = msg_send![path, fill];

        let _: () = msg_send![image, unlockFocus];
        let _: () = msg_send![image, setTemplate: true];
        image
    }
}

/// 隐藏 Dock 图标与应用菜单：`NSApplicationActivationPolicyAccessory`。
///
/// .app 打包后 `LSUIElement=true` 已生效，这里是为了 `cargo run` 直接跑
/// 二进制时行为一致。必须在 NSApp 就绪后（gpui `.run()` 回调内）调用。
#[cfg(target_os = "macos")]
pub(crate) fn set_accessory_policy() {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if !app.is_null() {
            // NSApplicationActivationPolicyAccessory = 1；返回值是 BOOL。
            let ok: bool = msg_send![app, setActivationPolicy: 1_isize];
            if !ok {
                eprintln!("[tray] setActivationPolicy(Accessory) 被拒绝");
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn set_accessory_policy() {}

# 环境要求与构建

> Bolt 的依赖、构建、打包成 `.app`、菜单栏托盘与首次构建注意事项。
> 返回总览见 [README](../README.md)。

## 环境要求

- Rust（edition 2024，工具链 1.98+）
- Node.js（`typescript-language-server` 通过 shebang 调用 `node`）
- 全局安装语言服务：

```bash
npm i -g typescript-language-server typescript
```

> **注意**：`typescript-language-server` 依赖 TypeScript 5.x 提供的 `tsserver.js`。TypeScript 7（Go 原生版本）已移除该文件，会导致 LSP 初始化失败，请确保使用 5.x。

## 构建与运行

```bash
cargo run                        # 调试运行
cargo build                      # 仅构建
cargo test --workspace           # 运行全部测试（bolt + crates/plux-js-manager）
cargo test -p plux-js-manager    # 只跑插件运行时那部分
./scripts/build-macos-app.sh          # 打包成 target/release/Bolt.app
./scripts/build-macos-app.sh --sign   # 打包 + ad-hoc 签名（绕过 Gatekeeper）
./scripts/build-macos-app.sh --clean  # 先 cargo clean 再完整打包
```

工程是 cargo workspace：根包 `bolt` 是应用本体，[crates/plux-js-manager](../crates/plux-js-manager) 是不依赖 gpui 的 JS 插件 manager（可在没有图形环境下单独测试）。`[patch]` / `[profile]` 只在 workspace 根生效。

### 依赖说明（无本地补丁）

项目**不再对 gpui-kit 生态打本地补丁**：Launcher 主搜索框与 Cmd+K 动作面板都用原生能力实现——搜索框是宿主自己的 `Input`（类型 token、粘贴识别、二次过滤全在宿主侧），列表是自绘的 `list` 模块（悬停不选中、单击选中、双击执行），所以不需要 `Command::searchable(false)` 这类绕过手段，也不需要 cargo-patch-crate 与 `patches/` 目录。

仓库仍保留两处 `[patch]`，但都是**版本改道**而非本地代码补丁：`rquickjs` 改道到 gpui-kit 仓库的兼容 facade（全图只链接一份 QuickJS）；`gpui-base` 改道到 registry 版，避免与 git 依赖产生两份不同 crate。

### 打包成 macOS .app

项目自带打包脚本 [scripts/build-macos-app.sh](../scripts/build-macos-app.sh)，产出的 `.app` 是标准 bundle 结构，双击可运行。依赖 [Info.plist](../packaging/macos/Info.plist) 和 [entitlements.plist](../packaging/macos/entitlements.plist)。

**目录结构**：

```text
packaging/macos/
├── Info.plist       # Bundle 元数据、LSUIElement（后台应用不占 Dock）
├── entitlements.plist  # 签名权限：QuickJS-JIT + 网络
├── Bolt.icns    # 应用图标（由 icon-1024.png 生成）
└── icon-1024.png    # 图标源文件（AI 生成 + 自动裁边 + squircle 蒙版）
scripts/
└── build-macos-app.sh  # 打包脚本
```

**Info.plist 关键项**：

| 字段 | 值 | 说明 |
| --- | --- | --- |
| `CFBundleIdentifier` | `com.bolt.app` | 修改成你自己的标识再分发 |
| `LSUIElement` | `true` | 后台代理应用：Dock 不显示、不在 Cmd+Tab 列表，窗口由托盘 / 快捷键唤起 |
| `LSMinimumSystemVersion` | `12.0` | 最低 macOS 版本 |

> **注意**：不要加 `LSBackgroundOnly`——它会禁止应用创建任何窗口（Launcher / 脚本管理器都弹不出来）。`LSUIElement` 才是「无 Dock 图标但能开窗」的正确键。

### 菜单栏托盘

应用启动后常驻菜单栏（闪电图标，NSStatusItem 模板图，自动适配深浅色），不占 Dock。托盘菜单：

- **打开启动器**：等价于 `Alt+Space`
- **脚本管理** / **新建脚本**：打开脚本管理器窗口
- **退出 Bolt**（⌘Q）

实现在 [src/app/tray.rs](../src/app/tray.rs)：objc2 raw `msg_send!` 直接建 NSStatusItem + NSMenu，菜单点击经 crossbeam channel 回到 app tick 分发（ObjC 回调不直接碰 gpui）。`cargo run` 时通过 `setActivationPolicy(Accessory)` 隐藏 Dock；打包后由 `LSUIElement` 覆盖同一件事。

**代码签名（可选）**：

- **ad-hoc 签名**（本地能跑，不能分发）：加 `--sign` 参数，脚本自动用 `-` 身份签名并带上 entitlements
- **开发者签名**（分发到其他 Mac）：需要 Apple Developer 账号。把 `codesign --sign -` 改成 `codesign --sign "Developer ID Application: Your Name (TEAMID)"`，然后用 `notarytool` 上传到 Apple 公证
- **未签名直接双击**：macOS Gatekeeper 会拒绝。右键 → 打开可跳过一次；或终端执行 `xattr -cr Bolt.app` 清除隔离属性

**首次运行的系统授权**：

打包后的 `.app` 首次调用宿主 API 时会自动弹窗请求：

- **辅助功能**（Accessibility）：`typeStr` 注入真实按键需要
- **屏幕录制**（Screen Recording）：`scanQr` 截屏解码 QR 需要

这两个权限无法在 Info.plist 里声明，必须用户手动在 **系统设置 → 隐私与安全性** 里授权。授权后杀掉进程重开即可生效。

**QuickJS-JIT 签名要求**：`entitlements.plist` 里声明了 `com.apple.security.cs.allow-unsigned-executable-memory`，因为 QuickJS-JIT 在进程内动态生成机器码。没有这个 entitlement，签名后的 App 一跑脚本就会 `SIGKILL`。

应用启动时会依次完成：**先建 Console 缓冲**（第一时间创建，启动期任何失败都进 Console 而不是只到 stderr——`.app` 双击启动时 stderr 用户看不到）→ 创建脚本目录、确定扩展目录（debug 直接用仓库 `extensions/`，release 用数据目录并播种仓库插件）→ 生成 `demo` 示例插件（目录已存在则不动）→ 装配插件宿主与 JS 运行时环境 → 注册内置 HostModule 并生成项目文件 → 启动语言服务 → 初始化 gpui-shell 运行时 → 注册全局热键后台待命（不打开窗口，按 `Alt + Space` 唤起 Launcher）。

`eprintln!` 只保留给「Console 本身还没建立」之前的兜底；此后所有启动期与运行期错误都走 `ConsoleChannel`（共享缓冲，容量 1000、溢出丢最旧；视图打开前的条目会留着，打开后一并显示；调试构建下还会同时打到终端）。`Error` 级条目另外同步落盘到数据目录的 `bolt.log`（滚动保留最近 500 行）——用户若从不打开 Console，日志文件就是启动失败唯一的出路。

> **首次构建**：UI 命令的运行时来自 git 依赖 `https://github.com/longbridge/gpui-kit`（tag `v0.7.3`，crates.io 上只有占位包），首次 `cargo build` 需要联网拉取该仓库与它嵌套的 `quickjs-jit`/LLRT，并编译 cranelift（JIT），明显慢于之后的增量构建。`Cargo.lock` 已提交，tag 不浮动。
>
> 普通脚本、插件运行时与 UI 命令**共用同一个 QuickJS**：`[patch.crates-io] rquickjs` 把 crates.io 的 `rquickjs` 改道到 gpui-kit 仓库里的兼容 facade（`pub use quickjs_jit::*`），因此二进制里不会链接两份 QuickJS 静态库。

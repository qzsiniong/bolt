# Bolt

基于 [gpui-kit](https://github.com/) 构建的 macOS 桌面应用，以「全局快捷键 + 命令面板」的方式运行 JavaScript / TypeScript 脚本，并附带一个脚本管理器（新建 / 编辑 / 删除）。

脚本以独立文件形式存储在磁盘上，配套自动生成的 `tsconfig.json` 与 `.d.ts` 类型声明，因此除了内置编辑器，也可以直接用 VSCode 等外部 IDE 打开脚本目录，获得完整的类型提示与跳转能力。

## 特性

- **全局快捷键 Launcher**：`Alt + Space` 唤起 Raycast 风格命令面板，搜索并运行脚本、扩展命令或打开管理器。
- **剪贴板粘贴识别**：Launcher 里 `Cmd + V` 自动识别剪贴板内容（图片 / 文件 / JSON / 链接 / 文本），搜索框出现带图标的类型 token，列表立即过滤为能处理该内容的命令；识别零转码、不读字节，毫秒级跟手。
- **扩展（plux-rs）**：`extensions/<id>-v<version>.js/` 目录就是一个扩展，清单是一份 `extension.json`；`commands` 里每条命令变成 Launcher 里的一条命令，磁盘改动下次唤出 Launcher 即生效。
- **两类命令一套清单**：命令带 `ui` 块 → gpui-shell 在原生窗口里渲染（入口固定 `main.js`，脚本用 `export default class extends View` 描述界面，无 WebView）；不带 → 在扩展自己的**常驻** QuickJS 运行时里执行同名导出函数，模块级状态跨命令保留。
- **fail-closed 扩展权限**：脚本命令要用的危险 API（文件读写、网络、exec、env、输入注入）必须在清单 `permissions` 里声明，缺省一个都不给；普通脚本全权（等同自己写的代码）。详见[信任模型](docs/trust-model.md)。
- **扩展管理与新建**：管理窗口一个列表看全扩展（版本、状态徽标、声明权限、被谁依赖、清单/运行错误），每个扩展可重载 / 禁用（**真不加载**）/ 在 Finder 中显示 / 打开数据目录 / 卸载（移到废纸篓）；「新建扩展」选模板（脚本 / UI / 两者）即生成带 `bolt.d.ts`、`gpui-kit.d.ts` 的可运行扩展目录，IDE 直接补全。
- **Dock 布局管理器**：左侧脚本列表、右上代码编辑器、右下 Console 输出面板，面板可拖拽重排。
- **内置编辑器**：Tree-sitter 语法高亮、行号、代码折叠、括号匹配。
- **完整 LSP 能力**：诊断、补全、悬停、跳转定义、Code Action，由 `typescript-language-server` 子进程驱动。
- **QuickJS 执行引擎**：进程内嵌入 `rquickjs`，无需 Node 运行时，脚本在独立线程执行，不阻塞界面。
- **TypeScript 直接运行**：通过 swc 做类型擦除（TS → JS）后交给 QuickJS 执行。
- **Rust 扩展模块（HostModule）**：Rust 侧注册模块后，脚本可用 ES module `import { x } from "name"` 直接调用，声明自动生成到 `types/<name>.d.ts`。
- **Console 面板与独立窗口**：按时间戳正序输出、等级着色（Log / Info / Warn / Error）、等级筛选、底部跟随滚动；脚本管理器的 dock 里有一份，Launcher 里搜「Console」还能开一个独立窗口。**调试构建下每条输出同时打到终端**（Info/Log → stdout，Warn/Error → stderr），`cargo run` 的窗口里就能直接看，不必来回开窗口。

## 快速开始

依赖 Rust（edition 2024，工具链 1.98+）与 Node.js，语言服务装一份（须 TypeScript 5.x，7.x 已移除 `tsserver.js` 会导致 LSP 失败）：

```bash
npm i -g typescript-language-server typescript
```

```bash
cargo run                            # 调试运行
cargo test --workspace               # 全部测试
./scripts/build-macos-app.sh --sign  # 打包成 target/release/Bolt.app（ad-hoc 签名）
```

首次 `cargo build` 需联网拉取 git 依赖 gpui-kit 并编译 cranelift（JIT），明显慢于之后的增量构建。环境细节、打包、托盘与系统授权见 [docs/build.md](docs/build.md)。

## 文档

| 文档 | 内容 |
| --- | --- |
| [Launcher 与扩展](docs/launcher.md) | Launcher 行为、Cmd+V 粘贴识别、应用启动器、Console、统一窗口壳、内置 JSON 编辑器 |
| [扩展（扩展系统）](docs/extensions.md) | `extension.json` 清单字段、脚本命令与 UI 命令、`ui.actions` 自定义按钮、JSON Schema、扩展管理与新建扩展 |
| [脚本 API](docs/script-api.md) | 普通脚本可用的运行时能力：`console`、定时器、`bolt` 内置模块、宿主交互 API |
| [脚本目录与快捷键](docs/scripts.md) | 磁盘布局与自动生成的类型声明；编辑器快捷键与多光标列选 |
| [在 Rust 侧扩展 JS 能力](docs/host-modules.md) | `HostModule` 框架：注册 Rust 模块供脚本 `import`，`.d.ts` 自动生成 |
| [代码结构与架构要点](docs/architecture.md) | `src/` 领域分层、各目录职责与贯穿性设计决策 |
| [环境要求与构建](docs/build.md) | 依赖、构建命令、打包 `.app`、菜单栏托盘、首次构建注意事项 |
| [信任模型](docs/trust-model.md) | 哪些代码默认拿什么权限、边界在哪 |

脚本存放在 `~/Library/Application Support/bolt/scripts/`，扩展在同级 `extensions/` 下——两者都会在首次启动自动创建，详见[脚本目录与快捷键](docs/scripts.md)。

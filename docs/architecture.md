# 代码结构与架构要点

> `src/` 的领域分层、各目录职责与贯穿性设计决策。
> 返回总览见 [README](../README.md)。

## 代码结构

`src/` 按领域分层，根目录只留 `main.rs` 与跨层共享的最小基础设施：

| 文件 / 目录 | 职责 |
| --- | --- |
| [src/main.rs](../src/main.rs) | 入口：仅 `mod` 声明 + `app::boot::run()` |
| [src/app/](../src/app) | 进程装配与窗口生命周期：`boot`（启动序列）、`shell`（AppShell 全局状态与共享服务）、`hotkey`（全局热键注册与轮询）、`launcher/`（应用主 UI：通用命令面板 + `LauncherItem`/`Module` 契约，不知道具体模块；拆成 `item` 契约、`collect` 收集排序、`filter` 查询/粘贴过滤、`actions` 动作构建、`list` 自绘列表（悬停不选中、单击选中、双击执行）、`panel` Cmd+K 浮层、`mod` 主视图与键盘分流；搜索框是宿主自己的 `Input`，类型 token、Cmd+V 粘贴识别、文本过滤与 ↑↓/Enter/Esc 键盘分流都在宿主侧完成）、`windows`（Launcher 窗口单例与被确认 item 的分发）、`assets`（内置图标 + 扩展目录的复合资源源）、`shell_log`（gpui-shell 的 tracing → Console 缓冲） |
| [src/modules/](../src/modules) | 内容模块，各自实现 `Module` trait 为 Launcher 提供 items：`apps/`（应用启动器：bundle 扫描、`InfoPlist.strings` 本地化解析、图标缓存、可编辑别名表）、`scripts/`（脚本管理器窗口 + 每条脚本一条运行命令）、`json_editor/`（原生 JSON 编辑器窗口）、`pin/`（「Pin 剪贴板」便签：把剪贴板图片/文本固定为桌面悬浮窗口，支持置顶、缩放、另存、复制回剪贴板；`clipboard.rs` 只做 pin 专属的一层转换——把宿主公共的 `PasteContent` 转成便签展示类型，并给出便签尺寸）、`console/`（`panel` 是 Console 视图：脚本管理器的 dock 面板与独立窗口共用同一个组件，`chrome` 决定要不要自己画标题栏；`window` 是独立窗口的开窗逻辑（单例），`mod` 提供 Launcher 里的「Console」命令）、`extensions/`（扩展：`manifest` 目录扫描与播种、`host` 与 plux 的 `Loader` 对接 + 扩展清单快照 `inventory()`、`js_env` 宿主注入、`exec` 脚本命令、`ui_window` UI 命令窗口、`prefs` 禁用列表持久化、`actions` 扩展级动作（Launcher 与管理窗口共用）、`ops` 重载/禁用/卸载等落地操作、`manager/` 扩展管理窗口、`create/` 新建扩展窗口、`scaffold` 模板生成与 `.d.ts` 落盘）。模块自己的窗口单例与专属资源放各自的 `state.rs`（Global） |
| [src/ui/](../src/ui) | 跨模块共享 UI 与纯前端逻辑：`window/`（统一窗口壳：`title_bar` 自绘标题栏构建器、`pin` 无状态置顶图钉、`open` 开窗样板与默认置顶、`level` NSWindow 层级 FFI）、`interaction_window`（宿主交互浮层）、`editor_ops`（不依赖 gpui 上下文的纯文本编辑操作）、`clipboard/`（NSPasteboard 读取与内容识别：`mod` 类型与 peek/read 入口、`files` Finder 文件条目、`html` 富文本转纯文本、`image` 格式与尺寸识别；`read_paste_content` 供 Launcher Cmd+V 与 pin 共用） |
| [src/model/](../src/model) | 领域数据与磁盘布局：`script`、`script_store`、`project_gen` |
| [src/runtime/](../src/runtime) | 执行链路：`runner`（普通脚本的 QuickJS 运行 + 扩展命令的 `run_extension`）、`interaction`（宿主交互桥）、`interaction_window_manager`（待命/进度/确认浮窗与 pickPath 选择器的状态机）、`run_service`（应用级运行服务：任务队列、取消与状态广播，`RunTask` 分脚本与扩展两种）、`ts_transform`（swc 类型擦除） |
| [src/host/](../src/host) | 宿主扩展框架（`HostModule` / `HostLoader` / `HostResolver`；模块转发源码与文件解析复用 `plux-js-manager` 的导出，与扩展侧同一份实现）与内置模块 `modules/{bolt,typer,qr}` |
| [src/lsp/](../src/lsp) | 语言服务器：`client`（请求管道与文档同步）、`server`（子进程与读写线程）、`protocol`（线协议与 URI 映射）、`providers`（桥接编辑器四大 provider） |
| [src/core/](../src/core) | 与领域无关的通用基础设施：`storage`（使用记录的数据形状、错误与 `UsageStorage` 抽象）、`file_storage`（JSON 文件实现，缺失文件视为空表、保存时自动建目录）、`smart_rank`（按时间衰减打分的排序引擎 + 事件裁剪）。依赖方向单向：`smart_rank` → `storage` ← `file_storage` |
| [crates/plux-js-manager/](../crates/plux-js-manager) | 独立的 JS 扩展 manager crate：`manifest`（`extension.json` 解析与校验）、`meta`（`plux_rs::Info` 实现）、`manager`（`Manager` trait）、`worker`（每扩展一条线程 + 常驻 QuickJS，四态就绪状态机）、`store`（跨线程执行入口 + 运行时健康状态）、`loader`（模块解析）、`env`（宿主注入点 `JsEnv`）、`schema`（清单 schema 的极简校验器，供测试用）。不依赖 gpui，可单独测试 |
| [schemas/extension.schema.json](../schemas/extension.schema.json) | 清单的 JSON Schema（手写，`include_str!` 进二进制）：给编辑器补全/校验用，宿主启动时复制到扩展根目录，运行时不读 |
| [src/console_channel.rs](../src/console_channel.rs) | 跨线程 Console 输出：`ConsoleChannel` 是写入端，`ConsoleLog` 是引用计数的共享缓冲（1000 条、溢出丢最旧），读者用 `since(cursor)` 取增量；`Error` 级条目同步落盘 `bolt.log`（滚动 500 行）；调试构建下每条输出同时打到 stdout/stderr。跨层共享的叶子模块 |
| [src/test_support.rs](../src/test_support.rs) | `#[cfg(test)]` 共享测试脚手架（`TempDir`、`position`、`run_js` headless 运行器） |

分组规则：`app` 管进程装配与 Launcher 主 UI（对具体模块一无所知），`modules` 放内容模块（各自向 Launcher 贡献 items、自管窗口与状态），`ui` 放跨模块共享控件与纯前端逻辑，`model` 放领域数据与磁盘布局，`runtime` 放执行链路，`host` 放宿主扩展框架及其背后的原生实现，`lsp` 放语言服务器客户端，`core` 放与领域无关、可单独测试的通用基础设施。新增模块只需在 `modules/` 下实现 `Module` trait 并加进 `default_modules()`。需要脱离 gpui 单独测试的能力（如扩展运行时）放 `crates/` 下的独立 crate。

## 架构要点

- **线程模型**：GPUI 主线程只负责渲染；脚本与扩展命令都在独立工作线程执行，输出经 `ConsoleChannel` 写进共享缓冲，视图按游标增量取。
- **一份缓冲，多个读者**：Console 输出原来走「有界 channel + 唯一 `Receiver`」——谁先建面板谁独占消费，后来者拿不到历史。现在写入端只往 `ConsoleLog` 里追加，读者各自持一个 `cursor` 调 `since(cursor)` 取增量：脚本管理器的 dock 面板与独立 Console 窗口因此能同时开着，关闭/重开也不影响对方，缓冲仍是固定容量（不用为了"多个读者"复制通道）。顺带把「满了丢最旧」从"发送方偷偷 pop 一端的 Receiver"改成缓冲自己的队列语义，写入路径不再出现读端操作。
- **三套运行时，一个引擎**：普通脚本在 worker 线程上跑一次性 rquickjs（可中断、可取消、可并发 await）；扩展脚本命令在**每扩展一条**的常驻线程上跑（`ExtensionStore::invoke` 同步调用，超时/取消靠 interrupt handler）；UI 命令在 GPUI 主线程上由 gpui-shell 执行并原生渲染。三者最终共用同一个 QuickJS（quickjs-jit），由 `[patch.crates-io] rquickjs` 保证。UI 命令的 `console` 与错误经 `tracing` 转到同一份 Console 缓冲。
- **扩展生命周期与执行分离**：plux 的 `Loader` 是 `&mut` 单线程的，只在主线程做「注册 / 安排加载 / 读元数据」；真正的命令执行交给 `ExtensionStore`（`Arc` 可跨线程）。`ensure_loaded` 只起线程不等待，等就绪发生在执行线程上，所以打开 Launcher 与第一次执行命令都不会卡住 UI。宿主另有一套健康检查：`extension.is_load` 说已加载、`ExtensionStore` 却说运行时已死时，先 `unload` 复位再重新安排——plux 的 `load_plugin` 对已加载扩展是 no-op，少了这一步就永远不会重建。
- **LSP 客户端**：`std::process` 启动子进程，用阻塞线程做 stdin 写入与 stdout 帧解析，与 GPUI 的 `background_spawn` 协作，不引入 tokio。子进程会注入 PATH，以保证从 Finder 启动时也能找到 `node`。
- **模块加载**：`HostLoader` 优先匹配已注册的 HostModule，其次按路径从磁盘加载（`.ts` 自动类型擦除）；扩展侧由 `ExtensionResolver` 把相对说明符解析成绝对路径，`lib/a.js` 里的 `import "./b.js"` 才会落到正确目录。两侧的「说明符 → 磁盘文件」与「宿主模块转发源码生成」是**同一份实现**（`plux_js_manager::{resolve_file, forwarding_module_source}`），不再各写一遍。转发模块是**转发函数**而不是一次性解构——宿主 API 分两批安装（加载时装基础函数、每轮命令前装交互 API），解构会把加载时的 `undefined` 固化进 const 绑定。
- **扩展权限的落点**：`HostModule::export_guarded` 登记每个导出所需的权限；`JsEnv::host_modules(permissions)` 按扩展清单把导出拆成「允许 / 拒绝」两半，拒绝项在转发模块里生成抛错桩（`import` 不炸，调用时点名缺哪个权限）。脚本链路不经过这道过滤——用户脚本全权。
- **禁用是数据层过滤，不是 UI 遮掩**：禁用集合在 `refresh` 里生效（被禁用的扩展整条跳过注册），`commands()` 与 `ensure_loaded()` 再各兜一道底。所以 Launcher、扩展管理窗口、RunService 三条路径自动一致——没有任何一处需要"记得过滤"。同理卸载的顺序就是正确性：先注销（plux 在被依赖时拒绝）再动磁盘，被依赖时磁盘一个字节都不会变。
- **别在 GPUI 回调里同步激活别的 App**：窗口事件处理器跑在 `App::update` 里（App 已被借用），此时若同步调 `-[NSWorkspace selectFile:…]` 之类的「切前台」API，AppKit 可能在自己的调用栈里就把 `windowDidResignKey` / 激活回调派发回来，GPUI 的回调路径再借一次 App → `RefCell already borrowed` 直接 abort（`cargo run` 里表现为 `failed to initiate panic, error 3`）。所以全项目只有一对入口：`mac_ffi::reveal_in_background`（选中）与 `mac_ffi::open_in_background`（打开），底层都是 `/usr/bin/open`（激活交给子进程与 launchd），线程与结果回调由它们内部管，调用方只管给一个错误处理闭包。四个调用点（扩展动作、新建扩展窗口、脚本列表、应用 / 脚本的 Cmd+K）都走这一对。
- **补全修正**：`typescript-language-server` 返回的候选带全量列表且无 `text_edit`，provider 侧按光标前缀过滤，并补齐 `text_edit` 范围，避免成员访问时点号被替换吞掉。

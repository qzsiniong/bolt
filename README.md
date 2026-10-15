# Scripter

基于 [gpui-kit](https://github.com/) 构建的 macOS 桌面应用，用于管理（新建 / 编辑 / 删除）与执行 JavaScript / TypeScript 脚本。

脚本以独立文件形式存储在磁盘上，配套自动生成的 `tsconfig.json` 与 `.d.ts` 类型声明，因此除了内置编辑器，也可以直接用 VSCode 等外部 IDE 打开脚本目录，获得完整的类型提示与跳转能力。

## 特性

- **Dock 布局界面**：左侧脚本列表、右上代码编辑器、右下 Console 输出面板，面板可拖拽重排。
- **内置编辑器**：Tree-sitter 语法高亮、行号、代码折叠、括号匹配。
- **完整 LSP 能力**：诊断、补全、悬停、跳转定义、Code Action，由 `typescript-language-server` 子进程驱动。
- **QuickJS 执行引擎**：进程内嵌入 `rquickjs`，无需 Node 运行时，脚本在独立线程执行，不阻塞界面。
- **TypeScript 直接运行**：通过 swc 做类型擦除（TS → JS）后交给 QuickJS 执行。
- **Rust 扩展模块（HostModule）**：Rust 侧注册模块后，脚本可用 ES module `import { x } from "name"` 直接调用，声明自动生成到 `types/<name>.d.ts`。
- **Console 面板**：按时间戳正序输出、等级着色（Log / Info / Warn / Error）、等级筛选、底部跟随滚动。

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
cargo run        # 调试运行
cargo build      # 仅构建
cargo test --bin scripter   # 运行测试
```

应用启动时会依次完成：创建脚本目录 → 注册内置 HostModule 并生成项目文件 → 启动语言服务 → 打开窗口。

## 脚本目录

macOS 下脚本存储在：

```
~/Library/Application Support/scripter/scripts/
```

首次启动会自动生成：

```
scripts/
├── tsconfig.json          # TS 配置（include scripts + types）
├── types/
│   ├── console.d.ts       # console 全局对象声明
│   ├── globals.d.ts       # 定时器全局声明
│   └── scripter.d.ts      # 内置 "scripter" 模块声明
├── hello.js               # 示例脚本（仅首次生成）
└── demo.ts                # 示例脚本（仅首次生成）
```

## 快捷键

| 快捷键 | 操作 |
| --- | --- |
| `Cmd + N` | 新建脚本 |
| `Cmd + S` | 保存当前脚本 |
| `Cmd + Enter` | 运行当前脚本 |

编辑器内（需编辑器聚焦）：

| 快捷键 | 操作 |
| --- | --- |
| `Cmd + /` | 切换行注释（`//`） |
| `Cmd + Option + /` | 切换块注释（`/* */`） |
| `Cmd + Shift + K` | 删除当前行（或选中行） |
| `Shift + Option + ↑` | 在上方复制选中行 |
| `Shift + Option + ↓` | 在下方复制选中行 |
| `Option + ↑` | 将选中行上移 |
| `Option + ↓` | 将选中行下移 |

### 多光标与列选（gpui-kit 内置）

编辑器引擎自带多光标与矩形列选能力，以下默认快捷键在编辑器聚焦时生效（macOS 的 Option 即 Alt 修饰键；Linux 的这些操作不使用 Super/Win）：

| 操作 | macOS | Linux | Windows |
| --- | --- | --- | --- |
| 在上方／下方添加光标 | `Cmd + Option + ↑ / ↓` | `Alt + Shift + ↑ / ↓` | `Ctrl + Alt + ↑ / ↓` |
| 逐字符扩展所有选区 | `Shift + ← / →` | `Shift + ← / →` | `Shift + ← / →` |
| 按词扩展所有选区 | `Option + Shift + ← / →` | `Ctrl + Shift + ← / →` | `Ctrl + Shift + ← / →` |
| 鼠标添加光标 | `Option` + 左键点击 | `Alt` + 左键点击 | `Alt` + 左键点击 |
| 矩形列选 | `Option + Shift` + 左键拖动 | `Alt + Shift` + 左键拖动 | `Alt + Shift` + 左键拖动 |
| 只保留活动光标 | `Escape` | `Escape` | `Escape` |

- Linux 额外支持与 Ghostty 一致的 `Ctrl + Alt` + 左键拖动列选，以及 `Alt + Shift + ← / →` 按词选择；Windows 额外支持 `Alt + Shift + ← / →` 逐字符选择。
- 三个平台都兼容 `Alt/Option` + 左键拖动列选：单击添加光标，继续拖动则以鼠标按下位置为起点建立新的矩形选区。
- 在编辑区按住 `Alt/Option` 时鼠标指针显示为 `+`；带 Alt 的选择手势优先于 `Ctrl/Cmd` + 点击跳转定义。
- 矩形选区按显示行生成，每行一个选区，短行会截断到已有文本边界；输入和删除同时作用于所有选区。
- 松开鼠标结束拖动；`Escape` 只保留活动光标（若上下文菜单已打开，则先处理菜单的 Escape）。
- 用 `↑ / ↓` 添加光标是累加操作，反向按键不会收缩矩形高度——这是多光标编辑与鼠标列选，并非持续的 Vim Visual Block 模式。
- 键盘输入期间光标保持可见，空闲 300ms 后恢复闪烁。
- Linux 桌面可能在编辑器收到事件之前拦截快捷键（部分桌面用 `Ctrl + Alt + ↑ / ↓` 切换工作区），因此 Linux 默认不绑定该组合。
- 以上快捷键指键盘重映射后的逻辑修饰键。

> 本项目以 macOS 为主，实际上生效的是 macOS 一列。注意 Linux 的 `Alt + Shift + ↑ / ↓`（框架用于添加上／下光标）与上文编辑器自定义的「复制选中行」（`Option + Shift + ↑ / ↓`）键位相同，在 Linux 上存在重叠。

## 脚本 API

### `console`

运行时注入的全局对象，输出被捕获到 Console 面板：

```ts
console.log('...');    // Log
console.info('...');   // Info
console.warn('...');   // Warn
console.error('...');  // Error
console.debug('...');  // Log
```

### 定时器

QuickJS 本身不提供定时器，运行时以宿主 API 的形式注入了浏览器风格的全局定时器：

```ts
declare function setTimeout(callback: () => void, delayMs?: number): number;
declare function clearTimeout(id?: number): void;
declare function setInterval(callback: () => void, periodMs?: number): number;
declare function clearInterval(id?: number): void;
```

回调在脚本顶层代码执行完毕后按延迟触发，定时器未结束前脚本不会退出。回调抛出的异常会输出到 Console 面板。

```js
console.log('start');
setTimeout(() => console.log('1s later'), 1000);   // 最后打印
console.log('end');                                 // 先于上一行打印
```

> `scripter` 模块的 `sleep(ms)` 同样基于 tokio 定时器，且支持 `Promise.all` 并行等待；需要等待且不关心回调风格时可优先使用。

### `scripter` 内置模块

```ts
import { readFile, writeFile, listDir, fetch, exec, env, exit } from 'scripter';

readFile(path: string): string;                 // 同步读取 UTF-8 文本
writeFile(path: string, content: string): void; // 同步写入
listDir(path: string): string[];                // 列出目录条目
fetch(url: string): Promise<{ status: number; body: string }>;                 // HTTP GET
exec(cmd: string): Promise<{ code: number; stdout: string; stderr: string }>;  // sh -c
env(key: string): string | undefined;           // 读取环境变量
exit(code: number): void;                       // 终止脚本
```

示例：

```ts
import { env, readFile } from 'scripter';

const name: string = env('USER') ?? 'world';
console.log(`hello, ${name}`);

const tsconfig = readFile('./tsconfig.json');
console.log(`tsconfig.json size: ${tsconfig.length} bytes`);
```

### 文件模块

除内置模块外，`import` 也支持相对路径与脚本目录下的裸路径，`.ts` 模块会自动类型擦除：

```ts
import { helper } from './utils.ts';
```

## 在 Rust 侧扩展 JS 能力

本项目通过 `HostModule` 框架实现「Rust 扩展 → JS `import` 调用」。无需改动 JavaScript/TypeScript 层，也无需发布 npm 包，全部在 Rust 侧完成注册即可。

### 实现原理

1. **HostModule 注册**：Rust 侧定义一个 `HostModule`，包含模块名、导出列表、TypeScript 声明、以及一个 `installer` 闭包。闭包在 QuickJS 上下文中创建一个以 `__<模块名>` 命名的全局对象，把 Rust 函数挂在上面。
2. **HostLoader 拦截 `import`**：rquickjs 的 `Loader` 在脚本执行 `import { foo } from "my-module"` 时，先查已注册的 HostModule。匹配成功后，`HostLoader` 动态生成一段微型 ES Module 源码：

   ```js
   const { foo } = globalThis.__my_module;
   export { foo };
   ```

   这段源码对 QuickJS 而言就是一个普通模块，因此 `import` 语法完全标准，无需 polyfill。
3. **类型声明自动生成**：每个 `HostModule` 的 `declaration` 字段会在启动时写入 `scripts/types/<name>.d.ts`。编辑器里的 `typescript-language-server` 读取这些声明后，补全、悬停、诊断直接生效。
4. **运行时装配**：脚本执行前，runner 依次调用 `registry.install_globals(&ctx)` → `scripter::install_console(&ctx)` → `scripter::install_timers(&ctx)`，把全部 Rust 函数和全局对象注入 QuickJS 上下文。

### 添加新模块的步骤

以添加一个 `"my-utils"` 模块为例（提供 `greet` 和 `now` 两个函数）：

#### Step 1：在 `src/host_modules/` 下新建文件

```rust
// src/host_modules/my_utils.rs
use rquickjs::{Ctx, Function, Object, Result};
use crate::host_module::{HostModule, HostRegistry};

const DECLARATION: &str = r#"
  /** Greet someone. */
  export function greet(name: string): string;
  /** Current UTC timestamp in milliseconds. */
  export function now(): number;
"#;

pub fn register(registry: &mut HostRegistry) {
    registry.register(
        HostModule::new("my-utils")
            .declaration(DECLARATION)
            .export("greet")
            .export("now")
            .installer(install),
    );
}

fn install<'js>(ctx: &Ctx<'js>) -> Result<()> {
    let g = Object::new(ctx.clone())?;

    // 同步函数：直接注册
    g.set("greet", Function::new(ctx.clone(), greet)?)?;
    // 异步函数：用 rquickjs::prelude::Async 包装，JS 侧得到 Promise
    g.set("now", Function::new(ctx.clone(), now)?)?;

    ctx.globals().set("__my_utils", g)?;
    Ok(())
}

fn greet(name: String) -> String {
    format!("Hello, {name}!")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
```

#### Step 2：在 `src/host_modules/mod.rs` 中注册

```rust
pub mod my_utils;

pub fn register_all(registry: &mut HostRegistry) {
    scripter::register(registry);
    my_utils::register(registry);   // ← 新增这一行
}
```

#### Step 3：JS 侧直接使用

```ts
import { greet, now } from 'my-utils';

console.log(greet('Scripter'));   // → "Hello, Scripter!"
console.log(now());               // → 1720000000000
```

无需重新配置 TypeScript，应用启动后 `scripts/types/my-utils.d.ts` 会自动生成，LSP 立即生效。

### 同步 vs 异步函数

| 类型 | Rust 签名 | 注册方式 | JS 返回值 |
| --- | --- | --- | --- |
| 同步 | `fn foo(args...) -> T` | `Function::new(ctx.clone(), foo)?` | 直接值 |
| 异步 | `async fn foo<'js>(ctx: Ctx<'js>, args...) -> Result<Value<'js>>` | `Function::new(ctx.clone(), Async(foo))?` | `Promise<T>` |

异步函数通过 `rquickjs::prelude::Async` 包装后，rquickjs 内部会把它转成返回 Promise 的函数。异步体内部可以用 `tokio::time::sleep`、`reqwest::get`、`tokio::process::Command` 等任意 tokio 异步 API，等待期间 QuickJS 引擎会继续调度其他 JS 任务（包括其他 Promise 回调和定时器）。

例如内置的 `scripter.fetch`：

```rust
use rquickjs::prelude::Async;

async fn fetch_url<'js>(ctx: Ctx<'js>, url: String) -> Result<Value<'js>> {
    let resp = reqwest::get(&url).await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    let body = resp.text().await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;

    let obj = Object::new(ctx.clone())?;
    obj.set("status", resp.status().as_u16() as i32)?;
    obj.set("body", body)?;
    obj.into_js(&ctx)
}
```

注册时：

```rust
g.set("fetch", Function::new(ctx.clone(), Async(fetch_url))?)?;
```

JS 侧：

```ts
import { fetch } from 'scripter';
const { status, body } = await fetch('https://example.com');
```

### 类型声明

`HostModule::declaration(...)` 接受一段 TypeScript 声明体，启动时自动包裹进：

```ts
declare module "my-utils" {
  // 你传入的内容放在这里
}
```

如果省略 `declaration`，框架会按 `.export("xxx")` 自动生成 `export const xxx: any;`（适合快速原型，但无类型信息）。推荐始终手写完整的 `declaration`，以获得 IDE 补全和诊断。

### 进阶：注入全局对象（非模块）

除了 `import` 模块，也可以直接在 QuickJS 全局命名空间注入对象。内置的 `console` 和定时器就是这样实现的——它们不是模块，而是运行时直接挂在 `globalThis` 上：

```rust
let console = Object::new(ctx.clone())?;
console.set("log", Function::new(ctx.clone(), |args: Rest<Value>| { ... })?)?;
ctx.globals().set("console", console)?;
```

这种方式适合需要「随处可用、无需 import」的 API（如 `console`、`setTimeout`）。模块方式则更适合命名空间隔离、按需导入的场景。

## 代码结构

| 文件 | 职责 |
| --- | --- |
| [src/main.rs](src/main.rs) | 入口：初始化目录、注册 HostModule、生成项目文件、启动 LSP 与窗口 |
| [src/workspace.rs](src/workspace.rs) | 顶层布局、面板订阅、全局快捷键、窗口标题 |
| [src/panels/](src/panels) | 脚本列表 / 编辑器 / Console 三个面板 |
| [src/script.rs](src/script.rs) · [src/script_store.rs](src/script_store.rs) | `Script` 模型与脚本目录读写（列表 / 新建 / 删除 / 重命名 / 保存） |
| [src/runner.rs](src/runner.rs) | 执行引擎：创建 QuickJS 运行时、装配 loader、求值脚本 |
| [src/ts_transform.rs](src/ts_transform.rs) | 基于 swc 的 TypeScript 类型擦除 |
| [src/host_module.rs](src/host_module.rs) | HostModule 注册框架 + rquickjs `Loader`/`Resolver` |
| [src/host_modules/scripter.rs](src/host_modules/scripter.rs) | 内置 `scripter` 模块与 `console` 全局注入 |
| [src/lsp.rs](src/lsp.rs) | 自研 LSP JSON-RPC 客户端（stdio + 阻塞线程读写） |
| [src/lsp_providers.rs](src/lsp_providers.rs) | 桥接 gpui 编辑器四大 provider，含补全过滤与 `text_edit` 修正 |
| [src/console_channel.rs](src/console_channel.rs) | 跨线程 Console 输出通道（crossbeam） |
| [src/project_gen.rs](src/project_gen.rs) | 生成 `tsconfig.json`、`.d.ts` 与示例脚本 |

## 架构要点

- **线程模型**：GPUI 主线程只负责渲染；脚本在独立工作线程执行，输出经 `ConsoleChannel` 回传面板。
- **LSP 客户端**：`std::process` 启动子进程，用阻塞线程做 stdin 写入与 stdout 帧解析，与 GPUI 的 `background_spawn` 协作，不引入 tokio。子进程会注入 PATH，以保证从 Finder 启动时也能找到 `node`。
- **模块加载**：`HostLoader` 优先匹配已注册的 HostModule，其次按路径从磁盘加载（`.ts` 自动类型擦除）。
- **补全修正**：`typescript-language-server` 返回的候选带全量列表且无 `text_edit`，provider 侧按光标前缀过滤，并补齐 `text_edit` 范围，避免成员访问时点号被替换吞掉。

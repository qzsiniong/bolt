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
│   ├── globals.d.ts       # 其他全局声明
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

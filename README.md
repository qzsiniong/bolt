# Scripter

基于 [gpui-kit](https://github.com/) 构建的 macOS 桌面应用，以「全局快捷键 + 命令面板」的方式运行 JavaScript / TypeScript 脚本，并附带一个脚本管理器（新建 / 编辑 / 删除）。

脚本以独立文件形式存储在磁盘上，配套自动生成的 `tsconfig.json` 与 `.d.ts` 类型声明，因此除了内置编辑器，也可以直接用 VSCode 等外部 IDE 打开脚本目录，获得完整的类型提示与跳转能力。

## 特性

- **全局快捷键 Launcher**：`Alt + Space` 唤起 Raycast 风格命令面板，搜索并运行脚本、扩展命令或打开管理器。
- **外部扩展机制**：`extensions/<id>/` 目录（单一 `extension.json` 清单 + 入口）即为一个扩展，按 `kind` 分为脚本式与 UI 两类。
- **UI 扩展（gpui-shell）**：清单里 `kind: "ui"`、入口固定为 `main.js`，脚本用 `export default class extends View` 描述界面，宿主在 GPUI 主线程渲染成独立原生窗口（无 WebView），支持热重载与清单声明置顶。
- **Dock 布局管理器**：左侧脚本列表、右上代码编辑器、右下 Console 输出面板，面板可拖拽重排。
- **内置编辑器**：Tree-sitter 语法高亮、行号、代码折叠、括号匹配。
- **完整 LSP 能力**：诊断、补全、悬停、跳转定义、Code Action，由 `typescript-language-server` 子进程驱动。
- **QuickJS 执行引擎**：进程内嵌入 `rquickjs`，无需 Node 运行时，脚本在独立线程执行，不阻塞界面。
- **TypeScript 直接运行**：通过 swc 做类型擦除（TS → JS）后交给 QuickJS 执行。
- **Rust 扩展模块（HostModule）**：Rust 侧注册模块后，脚本可用 ES module `import { x } from "name"` 直接调用，声明自动生成到 `types/<name>.d.ts`。
- **Console 面板**：按时间戳正序输出、等级着色（Log / Info / Warn / Error）、等级筛选、底部跟随滚动。

## Launcher 与扩展

应用启动后**不打开任何窗口**，仅注册全局热键后台待命。

| 快捷键 | 操作 |
| --- | --- |
| `Alt + Space` | 唤起 / 关闭 Launcher |
| `Enter` | 执行选中的命令 |
| `↑ / ↓` | 移动选择 |
| `Esc` | 清空搜索；搜索为空时关闭 Launcher |

Launcher 失焦后自动关闭。列表分三组：**内置**（脚本管理 / 新建脚本）、**脚本**（每个脚本一条命令，选中即运行）、**每个扩展一组**（脚本式扩展给出它声明的命令；UI 扩展给出一条「打开窗口」的命令）。

- **脚本管理**：打开脚本管理器窗口（单例，重复调用仅激活已有窗口）。
- **新建脚本**：打开管理器并直接进入新建流程。
- 脚本运行由应用级 RunService 执行，与管理器窗口无关——管理器关闭后从 Launcher 运行脚本，进度 / 确认浮窗与 Console 输出照常工作（Console 输出会缓冲，待管理器打开后可见）。

### 外部扩展

扩展存放在与 `scripts` 同级的目录：

```text
~/Library/Application Support/scripter/extensions/
└── demo/
    ├── extension.json    # 清单（必填）
    └── index.js          # 入口（默认 index.js，也支持 .ts）
```

`extension.json` 是**所有扩展共用的唯一清单**，一个字段区分两种形态：

```json
{
  "id": "demo",
  "name": "演示扩展",
  "description": "一句话说明",
  "kind": "script",
  "entry": "index.js",
  "commands": [
    { "id": "hello", "title": "打招呼", "subtitle": "运行 index.js" }
  ]
}
```

- `id` 必填，且必须与目录名一致；只允许 `a-z 0-9 . - _`，且首尾必须是字母或数字。
- `kind` 可省略，默认 `"script"`；`"ui"` 表示 UI 扩展（见下一节）。
- `entry` 可省略，脚本式默认 `index.js`（只支持 `.js` / `.ts`）；UI 扩展固定为 `main.js`。
- `commands` 可省略，省略时自动生成一条以扩展名命名的默认命令。
- 每条命令在 Launcher 中以扩展名作为分组标题出现，选中即运行入口脚本。
- 清单解析失败的目录会被跳过并在 Console 记录，不影响其他扩展。

扩展入口脚本与普通脚本拥有完全相同的能力：`console`、定时器、`scripter` 内置模块（`readFile` / `fetch` / `exec` 等）以及全部宿主交互 API（`typeStr` / `confirm` / `pickPath` / `scanQr` 等）。

### 仓库自带扩展

仓库的 `extensions/` 目录里放着随源码维护的扩展（目前是 `json-editor`）。用哪一份由构建类型决定：

- **debug 构建**：直接把仓库 `extensions/` 当作扩展目录，不做复制。改完仓库里的扩展文件重启即生效（UI 扩展在 debug 下还会热重载）；示例 `ui-demo` 只写在数据目录，不会污染工作树。
- **release 构建**：用用户数据目录，并把仓库扩展播种进去——只复制尚不存在的目录，不覆盖用户改动。

### UI 扩展（gpui-shell）

清单里 `kind` 为 `"ui"` 的目录是 **UI 扩展**：入口脚本只描述界面，宿主在 GPUI 主线程把它渲染成真实原生窗口（没有 WebView、没有 DOM/CSS/HTML）。

```text
~/Library/Application Support/scripter/extensions/
└── ui-demo/
    ├── extension.json    # 清单（必填，kind 为 ui）
    └── main.js           # 入口：export default class extends View
```

UI 扩展的清单沿用同一个 `extension.json`，只是把 `kind` 写成 `ui`：

```json
{
  "id": "ui-demo",
  "name": "UI 示例",
  "kind": "ui",
  "entry": "main.js",
  "capabilities": { "clipboard": true },
  "window": { "always_on_top": false, "width": 880, "height": 720 }
}
```

- **入口固定为 `main.js`**。UI 扩展的入口契约由宿主写死，因此 `entry` 只能写 `main.js`（写别的值会被报错并跳过），目录里也必须有 `main.js`。
- **清单由宿主校验**：`id` 必填且与目录名一致；`kind` 为 `ui`；`window.width/height` 必须为正数。
- **置顶（窗口层级）**：窗口一律以普通形态打开（`WindowKind::Normal`）；清单 `window.always_on_top`（默认 `false`）决定**开窗时**是否上浮。窗口右上角有宿主自绘标题栏，其中的图钉可随时切换置顶——Rust 侧直接改 NSWindow 层级（`NSFloatingWindowLevel` / `NSNormalWindowLevel`），不关窗、不重建脚本视图。
- `window.width` / `window.height` 默认 `880 × 720`，小于最小尺寸时以最小尺寸为准。

```js
import { View, div } from "gpui-kit";
import { h_flex, v_flex, Button, Input, InputState } from "gpui-base";

export default class UiDemo extends View {
  init() {
    this.count = 0;
    this.draft = InputState.new({ placeholder: "输入点什么…" });
  }

  render(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .size_full()
      .p(24)
      .gap(12)
      .bg(colors.background)
      .child(div().text_color(colors.foreground).child(`计数：${this.count}`))
      .child(Input.new(this.draft).h(28).px(8).border(1).border_color(colors.input));
  }
}
```

- 可用模块：`gpui-kit`（视图与调度）、`gpui-base`（基础控件：`Button` / `Link` / `Checkbox` / `Switch` / `Input` / `InputState` / `Textarea` / `TextareaState` / `TextView` / `h_flex` / `v_flex` …）、`gpui-fps`。样式全部写在脚本里（base 的控件不带视觉样式），颜色用 `cx.theme().colors` 的语义令牌。
- 状态就是视图字段，改完调用 `cx.notify()` 触发重绘。
- 窗口根就是 gpui-shell 的 `ShellRoot`，所以脚本能用 shell 自带的浮层与焦点能力：`window.open_dialog(...)`、`window.open_sheet(...)`、`window.push_toast({ title, level })`、`window.open_url(url)`、`cx.read_from_clipboard()` 等。
- 数据目录：`~/Library/Application Support/scripter/extensions-data/<id>/store.json`（`localStorage` 落在这里）。
- **权限由宿主按清单构建**：宿主始终授予「读扩展目录 + 读/写自己的数据目录 + `storage` + `exit`」；清单 `capabilities.clipboard`（默认 `false`）额外开关读写剪贴板。扩展目录被当作你自己写的代码，不做审批弹窗。
- **宿主自绘标题栏**：每个扩展窗口顶部由宿主提供一条标题栏（扩展名 + 右上角置顶图钉），下方才是扩展自己的界面。扩展脚本无需也无法自绘这一区域。
- 首次加载会在扩展目录自动生成 `gpui-kit.d.ts`；放一个 `jsconfig.json` 就能在 VSCode 里得到补全与类型检查。
- **debug 构建**下保存入口文件即热重载；加载失败会在窗口里显示可选中复制的错误面，同时把原因写进 Console 面板。
- `svg("icons/x.svg")` 相对**扩展目录**解析（不是相对当前文件），扩展目录里的同名文件会遮蔽内置 Lucide 图标。
- **旧布局会被报错并跳过**：一个 UI 扩展目录若只有 `gpui-shell.json` 而没有 `extension.json`，或两者同时存在（歧义），都会被跳过并在 Console 报错。
- 首次启动会生成 `ui-demo` 示例（目录已存在则不动），可直接在 Launcher 里跑通全链路。
- **尚未桥接**：UI 扩展目前不能调用普通脚本的宿主 API（`scripter` 模块的 `typeStr` / `confirm` / `pickPath` / `scanQr`）——两套运行时的桥接尚未实现。它们各自可用的能力（文件读写、`fetch`、WebSocket、`localStorage`、定时器）不受影响。

### 内置 JSON 编辑器

仓库自带一个用扩展机制实现的 JSON 编辑器，位于 [`extensions/json-editor`](extensions/json-editor)，启动时同步到用户扩展目录，在 Launcher 里以「JSON 编辑器」分组出现。它演示了 UI 扩展的完整用法（置顶窗口、剪贴板能力、`Textarea` / `TextView`、相对 `import` 分文件）。

- **置顶可运行时切换**：清单默认 `window.always_on_top: true`；点宿主标题栏右上角的图钉即可切换，窗口不关闭、内容不丢失。
- **粘贴自动格式化**：向编辑器粘贴一段 ≥8 字符的文本时自动解析并格式化（行号 / 语法高亮由只读结果面板的 Markdown 承担）。
- **容错解析**：不止标准 JSON，还能识别 JSONC / JSON5 / JS 字面量、URL 查询串、XML、YAML 子集，以及被转义的 JSON 字符串；解析失败时给出定位提示。
- **JS 过滤**：左下角输入框写受限表达式（`.key.subkey`、`[0][1]`、`.map(x => x.val)` 等，支持 `map` / `filter` / `find` / `slice` / `keys` / `values` / `sort` / `reverse` / `join` / `flat` 与比较运算），结果渲染在只读结果面板。求值器自研（沙箱里 `eval` / `Function` 被禁用），不是完整 JavaScript。
- **右下角工具**：格式化、压缩、压缩并复制、复制、转义 / 反转义字符串、从剪贴板导入、清空等。

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

应用启动时会依次完成：创建脚本目录、确定扩展目录（debug 直接用仓库 `extensions/`，release 用数据目录并播种仓库扩展与示例 UI 扩展）→ 注册内置 HostModule 并生成项目文件 → 启动语言服务 → 初始化 gpui-shell 运行时 → 注册全局热键后台待命（不打开窗口，按 `Alt + Space` 唤起 Launcher）。

> **首次构建**：UI 扩展运行时来自 git 依赖 `https://github.com/longbridge/gpui-kit`（tag `v0.7.3`，crates.io 上只有占位包），首次 `cargo build` 需要联网拉取该仓库与它嵌套的 `quickjs-jit`/LLRT，并编译 cranelift（JIT），明显慢于之后的增量构建。`Cargo.lock` 已提交，tag 不浮动。
>
> 脚本引擎与扩展运行时**共用同一个 QuickJS**：`[patch.crates-io] rquickjs` 把 crates.io 的 `rquickjs` 改道到 gpui-kit 仓库里的兼容 facade（`pub use quickjs_jit::*`），因此二进制里不会链接两份 QuickJS 静态库。

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

与 `scripts/` 同级的 `extensions/` 目录也会自动创建，用于存放外部扩展（见「Launcher 与扩展」）；UI 扩展的私有数据目录是再下一级的 `extensions-data/<id>/`。

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

### 宿主交互 API

脚本通过 `import from 'scripter'` 导入宿主提供的交互 API。调用时会自动弹出置顶小窗（进度条 / 待命提示 / 确认对话框），用户交互后继续执行。

```ts
import { typeStr, requestFocus, confirm, pickPath, scanQr } from 'scripter';
```

#### `typeStr(text: string, delayMs?: number, label?: string): void`

向当前焦点窗口注入真实按键。同步执行，每字符按 `delayMs` 间隔发送（默认 **300ms**）。支持 CJK 和特殊字符。

- `delayMs` 可选，不传默认 300ms。
- `label` 可选，自定义进度窗口标题，默认 "输入文本"。

首次调用前会弹「请点击目标窗口」的待命提示，本窗口失焦后自动开始输入。中途再次调用会直接注入。

```ts
// 默认 300ms/字符，标题 "输入文本"
typeStr('你好世界\n');
// 自定义间隔和标题
typeStr('vite build\n', 50, '执行构建命令');
```

#### `requestFocus(hint?: string): void`

显式请求用户确认目标窗口。弹出待命提示，用户点击目标窗口（本应用失焦）后返回。

```ts
requestFocus('请切换到浏览器窗口');
typeStr('https://example.com\n');
```

#### `confirm(message: string): Promise<boolean>`

弹出确认对话框（是 / 否），返回对应布尔值。

```ts
if (await confirm('确定要删除吗？')) {
  console.log('已删除');
}
```

#### `pickPath(prompt?: string, kind?: 'file' | 'dir'): Promise<string | null>`

弹出原生文件/目录选择器，返回选中的路径字符串；用户取消时返回 `null`。

```ts
const file = await pickPath('请选择一个图片文件', 'file');
if (file) console.log('选中：', file);
```

#### `scanQr(): Promise<string | null>`

截屏所有显示器并尝试解码 QR 码。返回第一个识别到的字符串；无码或解码失败返回 `null`。

```ts
const url = await scanQr();
console.log(url ?? '未识别到 QR 码');
```

#### 权限说明

- **辅助功能**：`typeStr` 需要「系统设置 → 隐私与安全性 → 辅助功能」授权 Scripter。
- **屏幕录制**：`scanQr` 需要「系统设置 → 隐私与安全性 → 屏幕录制」授权 Scripter。首次调用时系统会自动弹窗请求。

#### 注意事项

- `typeStr` 和 `requestFocus` 是同步阻塞调用；`confirm` / `pickPath` / `scanQr` 返回 Promise。
- 若未先调用 `requestFocus` 直接 `typeStr`，按键会自动落到当前焦点窗口（通常就是 Scripter 自身编辑器），因此首次使用建议先调用 `requestFocus`。
- 执行中点击交互窗的「中止」按钮，脚本会在下一个异步边界抛出 `AbortError` 样式的错误并停止。

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

#### Step 1：在 `src/host/modules/` 下新建文件

```rust
// src/host/modules/my_utils.rs
use rquickjs::{Ctx, Function, Object, Result};
use crate::host::{HostModule, HostRegistry};

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

#### Step 2：在 `src/host/modules/mod.rs` 中注册

```rust
pub(crate) mod my_utils;

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

`src/` 按领域分层，根目录只留 `main.rs` 与跨层共享的最小基础设施：

| 文件 / 目录 | 职责 |
| --- | --- |
| [src/main.rs](src/main.rs) | 入口：仅 `mod` 声明 + `app::boot::run()` |
| [src/app/](src/app) | 进程装配与窗口生命周期：`boot`（启动序列）、`shell`（AppShell 全局状态与共享服务）、`hotkey`（全局热键注册与轮询）、`launcher/`（应用主 UI：通用命令面板 + `LauncherItem`/`Module` 契约，不知道具体模块）、`windows`（Launcher 窗口单例与被确认 item 的分发）、`assets`（内置图标 + 扩展目录的复合资源源）、`shell_log`（gpui-shell 的 tracing → Console 面板） |
| [src/modules/](src/modules) | 内容模块，各自实现 `Module` trait 为 Launcher 提供 items：`scripts/`（脚本管理器窗口 + 每条脚本一条运行命令）、`json_editor/`（原生 JSON 编辑器窗口）、`extensions/`（`manifest` 清单扫描 + `ui_window` UI 扩展窗口）。模块自己的窗口单例与专属资源放各自的 `state.rs`（Global） |
| [src/ui/](src/ui) | 跨模块共享 UI：`interaction_window`（宿主交互浮层）、`window_pin`（置顶图钉公共控件）、`editor_ops`（不依赖 gpui 上下文的纯文本编辑操作） |
| [src/model/](src/model) | 领域数据与磁盘布局：`script`、`script_store`、`project_gen` |
| [src/runtime/](src/runtime) | 脚本执行链路：`runner`（QuickJS 运行）、`interaction`（宿主交互桥）、`run_service`（应用级运行服务）、`ts_transform`（swc 类型擦除） |
| [src/host/](src/host) | 宿主扩展框架（`HostModule` / `HostLoader` / `HostResolver`）与内置模块 `modules/{scripter,typer,qr}` |
| [src/lsp/](src/lsp) | 语言服务器：`client`（请求管道与文档同步）、`server`（子进程与读写线程）、`protocol`（线协议与 URI 映射）、`providers`（桥接编辑器四大 provider） |
| [src/console_channel.rs](src/console_channel.rs) | 跨线程 Console 输出通道（crossbeam），跨层共享的叶子模块 |
| [src/test_support.rs](src/test_support.rs) | `#[cfg(test)]` 共享测试脚手架（`TempDir`、`position`） |

分组规则：`app` 管进程装配与 Launcher 主 UI（对具体模块一无所知），`modules` 放内容模块（各自向 Launcher 贡献 items、自管窗口与状态），`ui` 放跨模块共享控件与纯前端逻辑，`model` 放领域数据与磁盘布局，`runtime` 放脚本执行链路，`host` 放宿主扩展框架及其背后的原生实现，`lsp` 放语言服务器客户端。新增模块只需在 `modules/` 下实现 `Module` trait 并加进 `default_modules()`。

## 架构要点

- **线程模型**：GPUI 主线程只负责渲染；脚本在独立工作线程执行，输出经 `ConsoleChannel` 回传面板。
- **两套运行时，一个引擎**：普通脚本在 worker 线程上跑 rquickjs（可中断、可取消、可并发 await）；UI 扩展在 GPUI 主线程上由 gpui-shell 执行并原生渲染。两者最终共用同一个 QuickJS（quickjs-jit），由 `[patch.crates-io] rquickjs` 保证。UI 扩展的 `console` 与错误经 `tracing` 转到同一个 Console 面板。
- **LSP 客户端**：`std::process` 启动子进程，用阻塞线程做 stdin 写入与 stdout 帧解析，与 GPUI 的 `background_spawn` 协作，不引入 tokio。子进程会注入 PATH，以保证从 Finder 启动时也能找到 `node`。
- **模块加载**：`HostLoader` 优先匹配已注册的 HostModule，其次按路径从磁盘加载（`.ts` 自动类型擦除）。
- **补全修正**：`typescript-language-server` 返回的候选带全量列表且无 `text_edit`，provider 侧按光标前缀过滤，并补齐 `text_edit` 范围，避免成员访问时点号被替换吞掉。

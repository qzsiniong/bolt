# 扩展（插件系统）

> Bolt 的插件机制：`extensions/` 目录、`extension.json` 清单、脚本命令与 UI 命令、
> 权限与信任边界见[信任模型](trust-model.md)。本文从 README 抽出，返回总览见
> [README](../README.md)。

## 插件（plux-rs）

插件存放在与 `scripts` 同级的目录，插件根就是 `extensions/` 本身：

```text
~/Library/Application Support/bolt/
├── extensions/
│   ├── demo-v1.0.0.js/              # 目录名 = <id>-v<version>.js
│   │   ├── extension.json              # 清单（必填）
│   │   ├── index.js                 # 脚本命令入口（默认 index.js，也支持 .ts）
│   │   └── main.js                  # UI 命令入口（有 ui 命令时才需要）
│   └── json-editor-v1.0.0.js/
└── extensions-data/demo/store.json  # 插件私有数据（localStorage 落这里）
```

**目录名就是 bundle 标识**。plux-rs 的 `Bundle` 完全从目录名解析出 `id` / `version` / `format`，所以：

- 目录名必须是 `<id>-v<version>.js`（`v` 前缀、语义化版本、`.js` 后缀，缺一不可，如 `demo-v1.0.0.js`）；
- 清单里的 `id` 与 `version` **可以省略**——目录名已经写着它们，再抄一遍只是冗余；写了就必须与目录名**逐字一致**，两边漂移的插件会被拒绝加载并在 Console 报错（这是作者改目录名时最容易漏的一步，所以保留这道检查）；
- `format` 后缀目前只认 `.js`，它是宿主挑 manager 的依据，与入口文件的扩展名不是一回事。

## `extension.json`

```json
{
  "id": "demo",
  "name": "演示插件",
  "description": "同时演示脚本命令与 UI 命令",
  "version": "1.0.0",
  "entry": "index.js",
  "commands": [
    { "id": "hello", "title": "打招呼", "description": "在 Console 里打印一行问候" },
    {
      "id": "panel",
      "title": "打开面板",
      "description": "在原生窗口里渲染一个计数器",
      "ui": { "title": "演示面板", "width": 480, "height": 320 }
    }
  ]
}
```

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `id` | | 插件标识；**省略时取目录名里的 id**，写了必须一致 |
| `version` | | 语义化版本；**省略时取目录名里的版本**，写了必须一致 |
| `name` | | 展示名，省略时退回 `id` |
| `description` | | 插件说明 |
| `entry` | | 脚本命令入口，默认 `index.js`，只接受 `.js` / `.ts`。**纯 UI 插件不需要**：没有脚本命令时这个字段根本不读，也不用在目录里放 `index.js` |
| `depends` / `optional_depends` | | `["base@^1.0"]` 形式的依赖声明，映射到 plux 的 `Depend`（不带 `@` 表示任意版本） |
| `permissions` | | 脚本命令要用的权限名，如 `["fs", "net"]`。**缺省一个都不给（fail-closed）**：没声明的危险 API 在调用时抛错，报错里会写明缺哪个权限。详见[信任模型](trust-model.md) |
| `commands` | | 命令数组；**省略时会自动补一条 id 为 `run`、标题为插件展示名的命令** |

`id` / `version` 是**唯一可以整行删掉**的字段：目录名 `<id>-v<version>.js` 是 plux bundle 的唯一定义处（`Loader` 只拿到路径，manager 没有覆盖 bundle 的机会）。因此最小清单就是一个 `{ }`——不过通常至少要写 `commands`，否则只有一条以插件名为标题的命令。

`commands[*]` 的字段：

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `id` | ✅ | 命令标识，插件内唯一；脚本命令里它就是入口模块要导出的函数名 |
| `title` | ✅ | Launcher 里显示的标题 |
| `description` | | 副标题；省略时显示「插件名 v版本」 |
| `ui` | | 给了就是 **UI 命令**（见下一节），省略就是 **脚本命令** |

`ui` 块（全部可省）：

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `title` | 同命令标题 | 窗口标题：既是系统窗口标题，也是宿主自绘标题栏上的主标题。插件名作为副标题跟在后面（与主标题相同时不重复） |
| `width` / `height` | `880` / `720` | 初始窗口尺寸，必须为正。小于宿主最小尺寸时以最小尺寸为准——**宽度还要放得下标题栏右侧的按钮**（每个动作约占 30pt，图钉常驻），否则图钉会被裁出窗口 |
| `always_on_top` | `false` | 开窗即上浮到 floating 层级（标题栏上的图钉仍可随时切换） |
| `capabilities.clipboard` | `false` | UI 命令能否读写剪贴板 |
| `actions` | `[]` | 宿主标题栏右侧的自定义按钮，每项 `{"builtin": "reload" \| "open_data_dir" \| "open_plugin_dir" \| "close_window"}` 或 `{"command": "<脚本命令id>"}`，可再加 `icon` / `tooltip`；声明顺序即渲染顺序，图钉固定在最右 |

`actions` 的两种按钮：

- **`builtin`**：宿主内建动作，即时生效。`reload` 走扩展管理同一条「重载」路径——**会关闭该插件的所有 UI 窗口（包括按钮所在的这个）**，重开命令即用新代码；`open_data_dir` / `open_plugin_dir` 拉起访达；`close_window` 关掉当前窗口。
- **`command`**：点击执行本清单里的一条**脚本命令**（必须存在且不能指向另一条 UI 命令，否则清单被拒绝加载）。它走插件运行时，输出与错误都进 Console——但**无法回写当前窗口的 UI**：gpui-shell 还没有宿主→插件的事件通道，窗口里的脚本视图与脚本命令是两套运行时。

每个按钮还可以覆盖外观（都可省略，省略即用该动作的默认值）：

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `icon` | builtin 各自的内置图标；`command` 用 ▶ | 写 Lucide 图标名（`zap`、`refresh-cw`，见 [Lucide](https://lucide.dev/icons)，1.9k+ 个）或插件目录里的资产路径（`icons/star.svg`，相对插件根；带 `/` 或以 `.svg` 结尾即按路径处理）。路径不能含 `..`，否则清单被拒。 |
| `tooltip` | builtin 的中文名；`command` 用命令标题 | 悬停提示文案，空白视同没写。 |

```json
"actions": [
  { "builtin": "reload", "icon": "zap", "tooltip": "重载并重开窗口" },
  { "builtin": "open_data_dir", "icon": "icons/folder-open.svg" },
  { "builtin": "close_window" }
]
```

图标走宿主的复合资产源：内置图标集兜底，插件自己 `icons/` 下的文件优先——同名文件会遮蔽内置图标（见 [src/app/assets.rs](../src/app/assets.rs)）。

清单里出现 `ui` 命令时，插件目录必须提供 `main.js`——gpui-shell 对没有 `gpui-shell.json` 的目录固定加载它，不可改名。

## 编辑器提示（JSON Schema）

清单的 schema 在 [schemas/extension.schema.json](../schemas/extension.schema.json)（Draft 2020-12）。宿主每次启动把它写到**扩展根目录**（`<extensions>/extension.schema.json`，与插件目录同级），于是插件里的清单只加一行就能在 VS Code 等编辑器里拿到字段补全、枚举候选与校验：

```json
{
  "$schema": "../extension.schema.json",
  "id": "demo"
}
```

- 宿主**不读** `$schema`（清单是宽松解析，未知字段直接忽略），删掉这一行不影响加载；
- 「新建扩展」生成的清单已经带上它；仓库里的示例插件也带着（`.vscode/settings.json` 另有一条映射，clone 下来不改任何文件也有提示）；
- schema 里 `additionalProperties: false`：字段名拼错（`alwaysOnTop` 这种）会当场标红。这类错误宿主是**静默忽略**的——写了不生效还不报错，交给编辑器发现最省事；
- 枚举都是逐字对齐的：`permissions` 的五项、`actions[*].builtin` 的四种（各带标题与说明）、`capabilities` 的开关；`actions` 还强制「`builtin` / `command` 恰好写一个」；
- 只有 `actions[*].icon` 没法把 1.9k+ 个 Lucide 名枚举进去，只校验「不含 `..`」：名字写错时按钮的图标会缺失（资产源解析不到，不会回退内置图标），这一点得靠肉眼或者试跑。

`schema` 与解析器的一致性由测试盯着（`plux-js-manager` 的枚举对齐测试、`extensions/schema_tests.rs` 用脚手架输出与示例插件反查 schema），改清单结构时不会只改一边。

## 脚本命令怎么执行

Launcher 里每条命令一条 item（id 是 `ext.<插件id>.<命令id>`，分组「扩展」）。选中脚本命令后，宿主在入口模块里按三档顺序找处理函数：

1. **具名导出**：`export function hello(ctx) { … }`（推荐）
2. **`export default` 对象**：`export default { hello(ctx) { … } }`
3. **全局挂载**：`globalThis.plux.commands.hello = (ctx) => …`

处理函数收到一个对象，返回的字符串会输出到 Console：

```js
export async function hello({ command, plugin, root, argv }) {
  // command: "hello"       plugin: { id: "demo", version: "1.0.0" }
  // root: 插件目录绝对路径   argv: 调用时传入的参数数组
  return `你好，这是 ${plugin.id} v${plugin.version} 的 ${command} 命令`;
}
```

- **每个插件一个常驻运行时**：第一条命令执行时才加载（插件顶层的副作用不会在启动时跑），之后一直活着，因此模块级状态跨命令保留——计数器、缓存、已经建好的连接都算。**加载不阻塞主线程**：主线程只把工人线程拉起来就返回，引擎构建与入口求值都在插件线程里进行，等待就绪发生在命令执行线程上（算进该次命令的超时预算）。
- **每插件一条专用线程**：plux 的 `Manager` / 函数调用是同步的，而宿主 API（定时器、`fetch`、`Async(...)`）依赖 tokio 反应堆，所以插件线程自带一个 current-thread 运行时，两者在同一线程里待着。GPUI 主线程只做「安排加载 + 投递命令」，从不等待引擎就绪，命令执行也从不占主线程。
- **单条命令超时 60 秒**。超时或用户点交互窗的「中止」都靠 QuickJS 的 interrupt handler 抛不可捕获异常，插件线程随后回到消息循环——`while (true) {}` 卡死不会拖垮应用，下一条命令照常能跑。
- **资源上限**：栈 8 MiB、堆 64 MiB；入口模块加载超时 30 秒。
- 命令返回后宿主给残留的定时器 / 未决 Promise 200 ms 预算跑完，不会因为一个没清掉的 `setInterval` 卡住下一次调用。
- **运行时死了会自动重建**：加载失败或线程退出后，下一次执行该命令时宿主先复位 plux 的 `is_load` 再重新安排（同一插件 3 秒冷却，防连点）。加载失败的原因经 `console` 落到面板，每次调用都会原样复述，不会退化成"线程已退出"这种含糊话。
- 插件命令里的 `exit(code)` 与脚本同义：`✔ Finished … (exit N)`，不是错误。

插件脚本与普通脚本的能力来源相同：`console`、定时器、`bolt` 内置模块以及全部宿主交互 API（`typeStr` / `requestFocus` / `confirm` / `pickPath` / `scanQr`）。差别在权限：**普通脚本全权**（脚本编辑器里看得见全部代码，等同你自己写的）；**插件脚本命令受清单 `permissions` 约束**，未声明的危险 API（`readFile` / `writeFile` / `listDir` / `fetch` / `exec` / `env` / `typeStr`）调用时抛错。交互 API 挂在常驻上下文上，每轮执行前用当轮的交互桥覆盖一次，所以进度窗、确认框、中止按钮都跟着当次命令走。

## 错误隔离与生效时机

- 清单解析失败、目录名不合约定、入口缺失、`id`/`version` 与目录名不一致 —— 该插件被跳过，原因写进 Console（`warn` 等级），其他插件不受影响。
- 每次唤出 Launcher 都重新扫描磁盘：**新增的插件目录下次唤出即可用**，不必重启应用；已注册的插件不会重复注册。**删掉的插件目录会在下次唤出时注销**，运行时随之下线。
- 同一个 id 装了多个版本时只保留**最高版本**的运行时与命令（item id 是 `ext.<id>.<command>`，不含版本）；低版本的注册保留给 plux 的依赖解析。
- **同 id 同版本但目录内容变了**（开发期最容易踩的坑：改完代码忘了升版本号）会被识别并自动重新注册：清单重读、运行时按新代码重建，Console 里留一条 info 提示“内容已变更”。判断依据是目录指纹（相对路径 + 大小 + 修改时间，跳过 `node_modules` 与生成的 `.d.ts`），内容没变就完全不动运行时——所以反复唤出 Launcher 不会打断正在用的插件。被已加载插件依赖着的插件不能这样重载，此时报 warn，升版本号才能生效。
- 加载失败（入口语法错误等）后修好文件，再执行一次命令即可重建运行时。
- 两种目录会被跳过：没有 `extension.json` 的目录（作者的草稿等）静默忽略；插件目录里出现 `gpui-shell.json` 会报错——它会让 gpui-shell 走严格清单路径，与宿主按 `extension.json` 安装的能力冲突。

## 管理、禁用与卸载

两个内置命令在 Launcher 里搜得到（分组「内置」）：**扩展管理**（`extensions.open-manager`）与**新建扩展**（`extensions.new`）。另外每条插件命令的 Cmd+K 动作面板里都带同一套**插件级动作**，不必先开管理窗口。

**管理窗口**（单例，960×640）：左侧列表显示展示名、版本与状态徽标（`已禁用` / `异常` / `被遮蔽` / `运行中` / `已退出`）；右侧详情是只读快照——注册状态、命令条数（其中 UI 命令几条）、声明了哪些权限、插件目录、数据目录、被谁依赖、清单错误、最近一次运行错误；底部操作条就是那套插件级动作。

渲染期**不扫盘**：`inventory()` 只遍历内存（上一次 `refresh` 的结果），所以窗口每帧重绘也不会碰磁盘；磁盘扫描和 Console 报告只发生在开窗、点「刷新」以及每次操作之后。

插件级动作固定为：

| 动作 | 行为 |
| --- | --- |
| 重新加载 | 注销旧条目 → 关掉该插件的 UI 窗口 → 重新扫描。改完清单/入口不想升版本号时用它，下一次执行命令就用新代码 |
| 禁用插件 / 启用插件 | 见下 |
| 在 Finder 中显示 | 打开插件目录所在位置 |
| 打开数据目录 | 打开 `extensions-data/<id>/`（不存在就建） |
| 卸载插件… | 确认框 → 关窗 → 注销 → **移到废纸篓** → 重新扫描 |

**禁用是真不加载**，不是把命令藏起来：禁用列表按插件 **id** 持久化在 `~/Library/Application Support/bolt/extensions-disabled.json`（`{ "disabled": ["demo"] }`），所以重装或升版本之后禁用状态仍然沿用。扫描时被禁用的插件**不注册**，`commands()` 与 `ensure_loaded()` 也各兜一道底——命令从 Launcher 消失，运行时不创建（已加载的靠注销顺带卸掉）。重新启用不需要重启，也不会多出一条「内容已变更」提示。

**被依赖时的两种处理，都不做半成功**：

- 禁用：注销时若 plux 拒绝（`CurrentlyUsesDepend`，说明有**已加载**的插件正依赖它），本次运行仍会注册，Console 说明「重启后不再加载」；命令照样从 Launcher 隐藏。
- 卸载：`forget` 先做（不碰磁盘），被依赖就直接报错中止——目录一个字节都不动，提示先卸依赖方。

**卸载走系统废纸篓**：`NSFileManager.trashItemAtURL:` 走 macOS 正统入口（可撤销、Finder 里能看到）；失败时（远程卷、权限）退回 `~/.Trash` 重命名，重名追加时间戳后缀；两条都失败就**不删**，只报错并保留原目录。确认框里会写清依赖方、以及 debug 构建下"仓库目录会出现在 git status 里"。

## 新建扩展

「新建扩展」是独立窗口（单例，640×560），填完直接落盘成一个可用的插件目录，**不需要重启也不需要再手写清单**：

- **id / 展示名 / 版本**：id 会被当作目录名，所以白名单是 `[a-z0-9][a-z0-9-]*`（≤64 字符、不能以 `-` 结尾）。校验的最后一步是把 `<id>-v<version>.js` 用 `Bundle` 解析回来**逐字比对**——`-v`、`.`、大写、空格这些会拆坏 bundle 的输入在这里就被挡住，而不是等下次扫描时在 Console 报错。
- **模板三选一**：脚本命令（`index.js` + 一条脚本命令）、UI 窗口（`main.js` + 一条 `ui` 命令）、两者都要（两个入口 + 两条命令）。
- **权限勾选**（脚本模板）：`fs` / `net` / `exec` / `env` / `typeStr`，不勾就是 fail-closed。
- **剪贴板授权**（UI 模板）：写进 `ui.capabilities.clipboard`。
- 表单实时显示目标目录、目录已存在的冲突，以及「同 id 已有版本」的提醒（多版本可以共存，但 Launcher 只显示最高版本）。
- **「创建」只在真能建的时候可点**：字段不合法、目标目录被占、或刚建完同一个目录时按钮置灰。创建成功后那行「目录已存在」不会留下来——刚建的那个目录不算冲突，否则成功提示旁边会挂一条红字，看着像失败。改 id 或版本号即恢复可点。
- 落盘中途失败（写文件/写声明出错）会把刚建的目录整个删掉，不留半成品占着名字，改完再点就能重试。
- **debug 构建下目标目录就是仓库 `extensions/`**：与宿主扫描的是同一份（`scaffold::target_dir` 跟着 `active_extensions_dir()` 走），建完立刻出现在 Launcher 与管理窗口里，界面同时提示它会出现在 `git status` 里；release 构建写用户数据目录 `~/Library/Application Support/bolt/extensions/`。

生成物自带类型提示，编辑器开箱即用（`.gitignore` 里也列了这三样，因为它们是生成物）：

| 文件 | 内容 |
| --- | --- |
| `bolt.d.ts` | 宿主模块声明（`declare module "bolt"`）：`typeStr` / `requestFocus` / `confirm` / `pickPath` / `scanQr` / 文件与 `fetch` 等，随宿主版本重写 |
| `gpui-kit.d.ts` | UI 命令的声明，仅在含 UI 命令时生成 |
| `jsconfig.json` | 只在目录里没有任何 jsconfig/tsconfig 时写一次（`checkJs` + `strict`），所以模板里的 JSDoc 是真有用的 |

纯 UI 模板不写 `bolt.d.ts`——那里根本没有 `bolt` 模块，写了只会让编辑器补全出一堆不存在的 API。

## 仓库自带插件

仓库的 `extensions/` 目录里放着随源码维护的插件（目前是 `json-editor`）。用哪一份由构建类型决定：

- **debug 构建**：直接把仓库 `extensions/` 当作扩展目录，不做复制，也不生成 `demo` 示例（不往工作树里写文件）。改完仓库里的插件文件，下次唤出 Launcher 即生效（同版本内容变更会被识别并重载），UI 命令的 `main.js` 还会热重载。
- **release 构建**：用用户数据目录，把仓库插件播种进去（只复制尚不存在的目录，不覆盖用户改动），并首次生成 `demo` 示例。

## UI 命令（gpui-shell）

命令带 `ui` 块就是 **UI 命令**：`main.js` 只描述界面，宿主在 GPUI 主线程把它渲染成真实原生窗口（没有 WebView、没有 DOM/CSS/HTML）。

```text
extensions/demo-v1.0.0.js/
├── extension.json    # 清单
├── index.js       # 脚本命令入口
└── main.js        # UI 命令入口：export default class extends View
```

同一个插件里可以既有脚本命令又有 UI 命令——区分只发生在命令一级：

```json
{
  "id": "demo",
  "version": "1.0.0",
  "commands": [
    { "id": "hello", "title": "打招呼" },
    {
      "id": "panel",
      "title": "打开面板",
      "ui": { "title": "演示面板", "width": 880, "height": 720, "always_on_top": true,
              "capabilities": { "clipboard": true } }
    }
  ]
}
```

- **入口固定为 `main.js`**：UI 入口由 gpui-shell 写死（对没有 `gpui-shell.json` 的目录只加载它），`entry` 字段管的是脚本命令的入口，与 UI 无关。
- **宿主校验**：有 `ui` 命令就必须有 `main.js`；`ui.width` / `ui.height` 必须为正数；同一插件里命令 id 不能重复。
- **置顶（窗口层级）**：窗口一律以普通形态打开（`WindowKind::Normal`）；`ui.always_on_top`（默认 `false`）决定**开窗时**是否上浮。标题栏右上角的图钉可随时切换置顶——Rust 侧直接改 NSWindow 层级（`NSFloatingWindowLevel` / `NSNormalWindowLevel`），不关窗、不重建脚本视图。
- `ui.width` / `ui.height` 默认 `880 × 720`，小于最小尺寸时以最小尺寸为准。

```js
import { View, div } from "gpui-kit";
import { h_flex, v_flex, Button, Input, InputState } from "gpui-base";

export default class DemoPanel extends View {
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
- 数据目录：`~/Library/Application Support/bolt/extensions-data/<id>/store.json`（`localStorage` 落在这里）。
- **权限由宿主按 `ui` 块构建**：宿主始终授予「读插件目录 + 读/写自己的数据目录 + `storage` + `exit`」；`ui.capabilities.clipboard`（默认 `false`）额外开关读写剪贴板。UI 命令不经 `bolt` 模块，不适用 `permissions` 声明。整体信任模型（哪些代码默认拿什么、边界在哪）见[信任模型](trust-model.md)。
- **宿主自绘标题栏**：每个 UI 命令的窗口顶部由宿主提供一条标题栏（插件名 + 右上角置顶图钉，图钉左边可放 `ui.actions` 声明的按钮），下方才是插件自己的界面。插件脚本无需也无法自绘这一区域。
- 首次加载会在插件目录自动生成 `gpui-kit.d.ts`；放一个 `jsconfig.json` 就能在 VSCode 里得到补全与类型检查。
- **debug 构建**下保存 `main.js` 即热重载；加载失败会在窗口里显示可选中复制的错误面，同时把原因写进 Console 面板。
- `svg("icons/x.svg")` 相对**插件目录**解析（不是相对当前文件），插件目录里的同名文件会遮蔽内置 Lucide 图标。
- 一个插件只保留一个 UI 窗口：再次执行同一条 UI 命令只是激活已有窗口。
- 首次启动（用数据目录的形态，即 release 构建）会生成 `demo-v1.0.0.js` 示例：脚本命令 + UI 命令各一条，目录已存在则不动，可直接在 Launcher 里跑通全链路。
- **尚未桥接**：UI 命令目前不能调用普通脚本的宿主 API（`bolt` 模块的 `typeStr` / `confirm` / `pickPath` / `scanQr`）——两套运行时的桥接尚未实现。它们各自可用的能力（文件读写、`fetch`、WebSocket、`localStorage`、定时器）不受影响。

# Launcher 与扩展

> Bolt 的交互入口与各内容窗口：Launcher 行为、粘贴识别、应用启动器、Console、
> 统一窗口壳、内置 JSON 编辑器。扩展机制细节见[扩展](extensions.md)，
> 返回总览见 [README](../README.md)。

应用启动后**不打开任何窗口**，仅注册全局热键后台待命。

| 快捷键 | 操作 |
| --- | --- |
| `Alt + Space` | 唤起 / 关闭 Launcher |
| `Cmd + V` | 识别剪贴板内容（图片/文件/JSON/链接/文本），出类型 token 并过滤命令 |
| `Enter` | 执行选中的命令 |
| `↑ / ↓` | 移动选择 |
| `Esc` | 清空搜索；搜索为空时关闭 Launcher |

Launcher 失焦后自动关闭。所有模块贡献的命令按「近期使用 + 使用次数」的衰减分数**全局平铺**（Raycast 风格，不分段），行尾带类型徽章：**内置**（脚本管理 / 新建脚本 / 扩展管理 / 新建扩展 / Console / 内置工具）、**应用**（本机安装的 macOS 应用）、**脚本**（每个脚本一条命令，选中即运行）、**扩展**（每条扩展命令一条：带 `ui` 的开窗口，其余的在扩展运行时里执行。

- **脚本管理**：打开脚本管理器窗口（单例，重复调用仅激活已有窗口）。
- **新建脚本**：打开管理器并直接进入新建流程。
- **扩展管理 / 新建扩展**：见[管理、禁用与卸载](extensions.md#管理禁用与卸载)与[新建扩展](extensions.md#新建扩展)；扩展级动作（重载 / 禁用 / 卸载等）在每条扩展命令的 Cmd+K 面板里也有一份。
- **Console**：打开独立的 Console 窗口（单例），见 [Console](#console)。
- 脚本运行由应用级 RunService 执行，与管理器窗口无关——管理器关闭后从 Launcher 运行脚本，进度 / 确认浮窗与 Console 输出照常工作（输出会进共享缓冲，打开 Console 窗口或管理器即可查看）。

## 粘贴识别（Cmd+V）

在 Launcher 里按 `Cmd + V`，先**识别**剪贴板里是什么，再决定行为：

- 识别出**图片 / 本地文件 / JSON / 链接 / 文本**时，搜索框出现一个带类型图标的原子 token（文件显示文件名，JSON/链接/文本显示前 12 个字符，截图等无名字节流显示"图片"），列表立即过滤为声明能处理该内容的命令（如「Pin 剪切板」便签、JSON 编辑器）。
- token 之后可以**继续输入文字二次过滤**；token 是原子单元，一次退格整体删除，删掉后列表恢复完整。
- 回车把剪贴板内容交给选中命令的处理器。图片字节此时才真正读取（识别阶段不读字节不转码），且在后台线程完成——回车后 Launcher 立即关闭，不卡 UI。
- 识别不出内容（空剪贴板等）时按普通粘贴处理。

**零转码原则**：gpui 原生解码 PNG/TIFF/JPEG/GIF，剪贴板里的原始字节+格式一路透传到渲染端（便签显示、另存、复制回剪贴板都保持原格式）——macOS 剪贴板图片最常见的 TIFF 不再做"解码→重编码 PNG"的无用功，这也是同类工具「从剪贴板固定」能做到瞬时出现的关键。

## 应用启动器

Launcher 的"应用"分组会在启动时扫描 `/Applications`、`/System/Applications`、`~/Applications` 下的 `.app`，按 canonical path 去重后生成命令项，用 `NSWorkspace` 拿到真实图标并缓存为 64×64 PNG。

**搜索匹配规则**（按优先级依次尝试）：

1. App 本地化显示名（`InfoPlist.strings` 里的 `CFBundleDisplayName`——中文系统下 `/Applications/WeChat.app` 会显示为"微信"而非 WeChat）
2. 原始文件名（去 `.app`）
3. Bundle Identifier（`com.tencent.xinWeChat`）
4. URL schemes（CFBundleURLTypes 里的 `weixin` / `wechat`）
5. 原始 Info.plist 值（`en.lproj` 等其他语言覆盖——企业微信会同时保留"WeCom"这个英文名）
6. 内置默认别名表（`weixin` → 企业微信等，编译进二进制）

**多关键字**：搜索词按空白切分（连续空格、首尾空白、全角空格都算分隔），**每个词都要命中**才算匹配，词序无关；每个词只要在标题 / id / 副标题 / 关键词任一字段里出现即可——「微信 wechat」能命中同一个 item，只要它有字段含"微信"、有字段含"wechat"（不必是同一字段）。只有空白等于没输入，列表不过滤。Cmd+K 动作面板沿用同一规则（只匹配动作标题）。

**拼音匹配**：含汉字的字段（标题、副标题、关键词）在匹配时自动派生「全拼」与「拼音首字母」两个变体——「企业微信」可被 `weixin` / `qiyeweixin` / `wx` / `qywx` 命中，混排文本取拼音 + 原字符（「JSON编辑器」→ `jsonbianjiqi` / `jsonbjq`）。派生发生在过滤层，**不写进关键词数据**：「编辑搜索关键词」回显与保存的仍是原始关键词。

**鼠标语义**（Raycast 同款，主列表与 Cmd+K 面板一致）：hover **只给一层淡高亮，不动选中**——鼠标掠过不会把键盘选中的行抢走；**单击选中**（并把焦点留在搜索框，接着打字不用再点一次）；**双击执行**，等同回车。列表是自绘的（[src/app/launcher/list.rs](../src/app/launcher/list.rs)），没有用 gpui-component 的 `Command`——它写死了「悬停即选中 + 单击即执行」且没有开关。键盘 ↑/↓ 移动选中时，新选中行会自动滚进视野。

**显示名解析**：按系统语言优先级找 `InfoPlist.strings`——先 `zh-Hans-CN` 再 `zh-Hans` 再 `Base` 最后 `en`。企业微信这种特殊布局（`zh-Hans.lproj` 目录存在但没有 `InfoPlist.strings`，`Base.lproj` 才有"企业微信"）也能正确取到中文名。

**自定义搜索词**：所有 item（应用、脚本、Pin 等）统一通过 Launcher 动作面板的「**编辑搜索关键词**」修改，保存到 `~/Library/Application Support/bolt/item-keywords.json`（按 item id 覆盖整个 keywords，多个词用逗号分隔，留空恢复默认）。应用会在此基础上叠加内置默认别名（源码维护在 `src/modules/apps/default-aliases.json`，`include_str!` 编进二进制）；编辑过的 item 以编辑值为准。

**使用统计**：排序不是简单的「用得多就靠前」，而是对每次确认记一个时间点、按 `Σ exp(-λ·距今天数)` 打分（默认只在最近 30 天内计分、90 天后丢弃），所以**最近用过**的会压过**很早以前用过很多次**的，长期不用则自然滑落。记录持久化在 `~/Library/Application Support/bolt/item-usage.json`（`item id → [使用时间点…]`），图标缓存放在 `app-icons/` 子目录。打分与裁剪实现在 [src/core/smart_rank.rs](../src/core/smart_rank.rs)。

## Console

输出只有一条链路：所有来源——脚本的 `console.*`、扩展命令与扩展运行时的输出、UI 扩展的 `tracing`、宿主自己的启动/操作/错误日志——都写进 `ConsoleChannel`，落到**一份共享缓冲**（最近 1000 条，溢出丢最旧）。`Error` 级条目另外滚动写入 `~/Library/Application Support/bolt/bolt.log`（保留 500 行），打包成 `.app` 后这是唯一的落盘线索。

**读者各持游标**：脚本管理器右下角的 Console 面板与独立的 Console 窗口读同一份缓冲，各自记住「读到哪儿」。两个可以同时开着、各自滚动、各看各的等级筛选，谁也不抢谁的输出。

| 场景 | 看哪里 |
| --- | --- |
| 写脚本时连跑几次 | 管理器里的 Console 面板（跟随最新，可按等级筛选） |
| 只想瞄一眼后台发生了什么 | Launcher 搜「Console」开独立窗口（单例，重复调用只激活已有窗口） |
| 调试构建、人在终端 | 每条输出同时打到终端，见下 |

**终端镜像**（调试构建默认开）：每条输出在写进缓冲的同时也打到终端——`Log` / `Info` 走 **stdout**，`Warn` / `Error` 走 **stderr**，逐行 flush（崩溃或退出前的最后一条不会卡在缓冲里丢掉）。所以 `cargo run 2>/dev/null` 只留正常输出，`2>&1 | grep '^\[x\]'` 能单独捞错误。`BOLT_CONSOLE_TERMINAL=0` 关掉；`=1` 让 release 构建也打开（`.app` 没有终端，默认关）。

窗口工具栏与面板一致（等级筛选 + `Clear`）。`Clear` 只清当前视图：缓冲里仍有历史，之后再开窗口还能看到它们。

独立窗口的标题栏是[统一窗口壳](#窗口与标题栏)的 `WindowTitleBar`（标题 + 「N 条输出」副标题 + 置顶图钉），并且**默认就置顶**：调试时它是个常驻仪表盘，被编辑器或浏览器盖住就没用了。点一下图钉落到普通层级，再点抬回 floating。层级是 NSWindow 的运行时消息，切换不重建窗口；dock 里的面板跟着脚本管理器窗口走，没有自己的层级，所以没有图钉。置顶状态不跨「关窗再开」保留——每次开窗都回到默认的置顶。

## 窗口与标题栏

所有带标题栏的内容窗口（脚本管理器、Console、JSON 编辑器、扩展管理、新建扩展、扩展 UI 窗口）共用 [`src/ui/window/`](../src/ui/window) 一套壳：

- **`WindowTitleBar`**：自绘标题栏构建器（13px 标题 + 可选 11px 副标题竖排 + 右侧控件），内部包 gpui-component 的 `TitleBar`——拖动、双击跟随系统设置、红绿灯让位都由它处理，本层只统一长相。
- **`WindowPin`**：置顶图钉，**无自有状态**——渲染读 NSWindow 真实层级，点击翻转层级。「窗口是否置顶」只有这一个真相，图标显示的状态不可能与真实层级漂移。
- **`WindowSpec` / `open_singleton`**：开窗样板收敛（激活已有 / 失效重建 + 尺寸/最小尺寸/系统标题）。「默认置顶」只在开窗参数 `WindowSpec.pinned(...)` 声明一处，开窗时、创建视图之前就把层级抬上去。
- **`level`**：唯一的 NSWindow 层级 FFI（置顶 = floating 层级 3），便签的圆按钮图钉也共用这一份。

| 窗口 | 默认置顶 | 说明 |
| --- | --- | --- |
| Console | **是** | 调试时的常驻仪表盘 |
| 脚本管理器 | 否 | 主窗口，置顶会挡别的应用 |
| JSON 编辑器 / 扩展管理 / 新建扩展 | 否 | 普通内容窗口 |
| 扩展 UI 窗口 | `ui.always_on_top`（默认否） | 扩展清单决定 |

**不参与统一壳的窗口**（各有结构性理由，别顺手改造）：Launcher（无边框 Floating 面板、toggle 语义）、通知与取色（PopUp + FFI 改 styleMask 的特殊窗口）、交互提示浮窗（PopUp 非激活面板，系统标题栏）、便签（borderless，保留黑底圆形按钮视觉，但置顶状态共用同一份层级真相）。

## 扩展（扩展系统）

扩展存放在与 `scripts` 同级的 `extensions/` 目录，目录名就是 bundle 标识（`<id>-v<version>.js`），清单是 `extension.json`。命令分两类：**脚本命令**在每扩展一条的常驻 QuickJS 线程里执行（受清单 `permissions` 约束，fail-closed），**UI 命令**由 gpui-shell 在 GPUI 主线程渲染成原生窗口（入口固定 `main.js`，无 WebView）。宿主每次唤出 Launcher 都重新扫描磁盘——新增、内容变更、删除都会被识别，禁用与卸载在扩展管理窗口或 Cmd+K 面板里完成，卸载走系统废纸篓。

完整的清单字段参考、`ui.actions` 自定义按钮（图标 / tooltip）、JSON Schema 编辑器提示、脚本命令执行模型、错误隔离与生效时机、扩展管理与新建扩展窗口、UI 命令（gpui-shell）开发指南，见 **[docs/extensions.md](extensions.md)**。

## 内置 JSON 编辑器

仓库自带一个用扩展机制实现的 JSON 编辑器，位于 [`extensions/json-editor`](../extensions/json-editor)，启动时同步到用户扩展目录，在 Launcher 里以「JSON 编辑器」一条命令出现。它演示了 UI 命令的完整用法（置顶窗口、剪贴板能力、`Textarea` / `TextView`、相对 `import` 分文件）。

- **置顶可运行时切换**：清单默认 `ui.always_on_top: true`；点宿主标题栏右上角的图钉即可切换，窗口不关闭、内容不丢失。
- **粘贴自动格式化**：向编辑器粘贴一段 ≥8 字符的文本时自动解析并格式化（行号 / 语法高亮由只读结果面板的 Markdown 承担）。
- **容错解析**：不止标准 JSON，还能识别 JSONC / JSON5 / JS 字面量、URL 查询串、XML、YAML 子集，以及被转义的 JSON 字符串；解析失败时给出定位提示。
- **JS 过滤**：左下角输入框写受限表达式（`.key.subkey`、`[0][1]`、`.map(x => x.val)` 等，支持 `map` / `filter` / `find` / `slice` / `keys` / `values` / `sort` / `reverse` / `join` / `flat` 与比较运算），结果渲染在只读结果面板。求值器自研（沙箱里 `eval` / `Function` 被禁用），不是完整 JavaScript。
- **右下角工具**：格式化、压缩、压缩并复制、复制、转义 / 反转义字符串、从剪贴板导入、清空等。

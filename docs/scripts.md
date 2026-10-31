# 脚本目录与快捷键

> 脚本在磁盘上的布局、自动生成的类型声明，以及编辑器与多光标快捷键。
> 脚本可用的 API 见[脚本 API](script-api.md)，返回总览见 [README](../README.md)。

## 脚本目录

macOS 下脚本存储在：

```text
~/Library/Application Support/bolt/scripts/
```

首次启动会自动生成：

```text
scripts/
├── tsconfig.json          # TS 配置（include scripts + types）
├── types/
│   ├── console.d.ts       # console 全局对象声明
│   ├── globals.d.ts       # 定时器全局声明
│   └── bolt.d.ts      # 内置 "bolt" 模块声明
├── hello.js               # 示例脚本（仅首次生成）
└── demo.ts                # 示例脚本（仅首次生成）
```

与 `scripts/` 同级的 `extensions/` 目录也会自动创建，扩展直接放在它下面：`<id>-v<version>.js/`（见 [Launcher 与扩展](launcher.md)），UI 命令的私有数据目录是 `extensions-data/<id>/`。应用启动器的相关数据也放在这里：`item-usage.json`（item 使用统计：id → 使用时间点列表）、`item-keywords.json`（用户编辑的搜索关键词）、`app-icons/`（图标缓存）、`bolt.log`（`Error` 级 Console 条目的落盘文件，滚动保留最近 500 行，排查启动失败用它）。

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

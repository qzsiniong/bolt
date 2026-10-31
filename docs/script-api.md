# 脚本 API

> 普通脚本（脚本管理器里的 `.js` / `.ts`）可用的全部运行时能力。
> 扩展命令的执行模型与权限差异见[扩展](extensions.md#脚本命令怎么执行)。
> 返回总览见 [README](../README.md)。


## `console`

运行时注入的全局对象，输出进 Console 缓冲（输出进 Console 缓冲：管理器面板、独立窗口、调试构建下的终端镜像都读它，见 [Launcher 与扩展](launcher.md#console)）：

```ts
console.log('...');    // Log
console.info('...');   // Info
console.warn('...');   // Warn
console.error('...');  // Error
console.debug('...');  // Log
```

## 定时器

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

> `bolt` 模块的 `sleep(ms)` 同样基于 tokio 定时器，且支持 `Promise.all` 并行等待；需要等待且不关心回调风格时可优先使用。

## `bolt` 内置模块

```ts
import { readFile, writeFile, listDir, fetch, exec, env, exit } from 'bolt';

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
import { env, readFile } from 'bolt';

const name: string = env('USER') ?? 'world';
console.log(`hello, ${name}`);

const tsconfig = readFile('./tsconfig.json');
console.log(`tsconfig.json size: ${tsconfig.length} bytes`);
```

## 文件模块

除内置模块外，`import` 也支持相对路径与脚本目录下的裸路径，`.ts` 模块会自动类型擦除：

```ts
import { helper } from './utils.ts';
```

## 宿主交互 API

脚本通过 `import from 'bolt'` 导入宿主提供的交互 API。调用时会自动弹出置顶小窗（进度条 / 待命提示 / 确认对话框），用户交互后继续执行。

```ts
import { typeStr, requestFocus, confirm, pickPath, scanQr } from 'bolt';
```

### `typeStr(text: string, delayMs?: number, label?: string): void`

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

### `requestFocus(hint?: string): void`

显式请求用户确认目标窗口。弹出待命提示，用户点击目标窗口（本应用失焦）后返回。

```ts
requestFocus('请切换到浏览器窗口');
typeStr('https://example.com\n');
```

### `confirm(message: string): Promise<boolean>`

弹出确认对话框（是 / 否），返回对应布尔值。

```ts
if (await confirm('确定要删除吗？')) {
  console.log('已删除');
}
```

### `pickPath(prompt?: string, kind?: 'file' | 'dir'): Promise<string | null>`

弹出原生文件/目录选择器，返回选中的路径字符串；用户取消时返回 `null`。

```ts
const file = await pickPath('请选择一个图片文件', 'file');
if (file) console.log('选中：', file);
```

### `scanQr(): Promise<string | null>`

截屏所有显示器并尝试解码 QR 码。返回第一个识别到的字符串；无码或解码失败返回 `null`。

```ts
const url = await scanQr();
console.log(url ?? '未识别到 QR 码');
```

### 权限说明

- **辅助功能**：`typeStr` 需要「系统设置 → 隐私与安全性 → 辅助功能」授权 Bolt。
- **屏幕录制**：`scanQr` 需要「系统设置 → 隐私与安全性 → 屏幕录制」授权 Bolt。首次调用时系统会自动弹窗请求。

### 注意事项

- `typeStr` 和 `requestFocus` 是同步阻塞调用；`confirm` / `pickPath` / `scanQr` 返回 Promise。
- 若未先调用 `requestFocus` 直接 `typeStr`，按键会自动落到当前焦点窗口（通常就是 Bolt 自身编辑器），因此首次使用建议先调用 `requestFocus`。
- 执行中点击交互窗的「中止」按钮，脚本会在下一个异步边界抛出 `AbortError` 样式的错误并停止。

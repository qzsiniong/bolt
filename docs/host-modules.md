# 在 Rust 侧扩展 JS 能力

> `HostModule` 框架：注册 Rust 模块供脚本 `import`，声明自动生成 `.d.ts`。
> 返回总览见 [README](../README.md)。


本项目通过 `HostModule` 框架实现「Rust 扩展 → JS `import` 调用」。无需改动 JavaScript/TypeScript 层，也无需发布 npm 包，全部在 Rust 侧完成注册即可。

## 实现原理

1. **HostModule 注册**：Rust 侧定义一个 `HostModule`，包含模块名、导出列表、TypeScript 声明、以及一个 `installer` 闭包。闭包在 QuickJS 上下文中创建一个以 `__<模块名>` 命名的全局对象，把 Rust 函数挂在上面。
2. **HostLoader 拦截 `import`**：rquickjs 的 `Loader` 在脚本执行 `import { foo } from "my-module"` 时，先查已注册的 HostModule。匹配成功后，`HostLoader` 动态生成一段微型 ES Module 源码：

   ```js
   const { foo } = globalThis.__my_module;
   export { foo };
   ```

   这段源码对 QuickJS 而言就是一个普通模块，因此 `import` 语法完全标准，无需 polyfill。
3. **类型声明自动生成**：每个 `HostModule` 的 `declaration` 字段会在启动时写入 `scripts/types/<name>.d.ts`。编辑器里的 `typescript-language-server` 读取这些声明后，补全、悬停、诊断直接生效。
4. **运行时装配**：脚本执行前，runner 依次调用 `registry.install_globals(&ctx)` → `bolt::install_console(&ctx)` → `bolt::install_timers(&ctx)`，把全部 Rust 函数和全局对象注入 QuickJS 上下文。

## 添加新模块的步骤

以添加一个 `"my-utils"` 模块为例（提供 `greet` 和 `now` 两个函数）：

### Step 1：在 `src/host/modules/` 下新建文件

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

### Step 2：在 `src/host/modules/mod.rs` 中注册

```rust
pub(crate) mod my_utils;

pub fn register_all(registry: &mut HostRegistry) {
    bolt::register(registry);
    my_utils::register(registry);   // ← 新增这一行
}
```

### Step 3：JS 侧直接使用

```ts
import { greet, now } from 'my-utils';

console.log(greet('Bolt'));   // → "Hello, Bolt!"
console.log(now());               // → 1720000000000
```

无需重新配置 TypeScript，应用启动后 `scripts/types/my-utils.d.ts` 会自动生成，LSP 立即生效。

## 同步 vs 异步函数

| 类型 | Rust 签名 | 注册方式 | JS 返回值 |
| --- | --- | --- | --- |
| 同步 | `fn foo(args...) -> T` | `Function::new(ctx.clone(), foo)?` | 直接值 |
| 异步 | `async fn foo<'js>(ctx: Ctx<'js>, args...) -> Result<Value<'js>>` | `Function::new(ctx.clone(), Async(foo))?` | `Promise<T>` |

异步函数通过 `rquickjs::prelude::Async` 包装后，rquickjs 内部会把它转成返回 Promise 的函数。异步体内部可以用 `tokio::time::sleep`、`reqwest::get`、`tokio::process::Command` 等任意 tokio 异步 API，等待期间 QuickJS 引擎会继续调度其他 JS 任务（包括其他 Promise 回调和定时器）。

例如内置的 `bolt.fetch`：

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
import { fetch } from 'bolt';
const { status, body } = await fetch('https://example.com');
```

## 类型声明

`HostModule::declaration(...)` 接受一段 TypeScript 声明体，启动时自动包裹进：

```ts
declare module "my-utils" {
  // 你传入的内容放在这里
}
```

如果省略 `declaration`，框架会按 `.export("xxx")` 自动生成 `export const xxx: any;`（适合快速原型，但无类型信息）。推荐始终手写完整的 `declaration`，以获得 IDE 补全和诊断。

这些声明在扩展目录里落成 `bolt.d.ts`（由 `HostRegistry::write_type_declarations` 汇总全部宿主模块）。「新建扩展」生成插件时会自动写一份，UI 命令另配 `gpui-kit.d.ts` 与 `jsconfig.json`——详见[新建扩展](extensions.md#新建扩展)。

## 进阶：注入全局对象（非模块）

除了 `import` 模块，也可以直接在 QuickJS 全局命名空间注入对象。内置的 `console` 和定时器就是这样实现的——它们不是模块，而是运行时直接挂在 `globalThis` 上：

```rust
let console = Object::new(ctx.clone())?;
console.set("log", Function::new(ctx.clone(), |args: Rest<Value>| { ... })?)?;
ctx.globals().set("console", console)?;
```

这种方式适合需要「随处可用、无需 import」的 API（如 `console`、`setTimeout`）。模块方式则更适合命名空间隔离、按需导入的场景。

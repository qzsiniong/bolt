//! TypeScript → JavaScript 的类型擦除。
//!
//! 用 swc 解析 TypeScript、去掉类型标注、再输出 JavaScript。`import` 语句原样
//! 保留，交给 rquickjs 的模块 loader 在运行时解析。不做 ES 版本降级。

use swc_common::{FileName, GLOBALS, Globals, Mark, SourceMap, sync::Lrc};
use swc_ecma_ast::{EsVersion, Program};
use swc_ecma_codegen::to_code_default;
use swc_ecma_parser::{Syntax, TsSyntax, parse_file_as_program};
use swc_ecma_transforms_base::{fixer::fixer, resolver};
use swc_ecma_transforms_typescript::strip;

/// 把 `ts_source` 里的 TypeScript 类型标注剥掉，返回保留 `import`/`export`
/// 的合法 JS 源码。
pub fn strip_typescript(ts_source: &str) -> Result<String, String> {
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(
        FileName::Custom("input.ts".into()).into(),
        ts_source.to_string(),
    );

    let mut recovered = Vec::new();
    let program: Program = parse_file_as_program(
        &fm,
        Syntax::Typescript(TsSyntax::default()),
        EsVersion::latest(),
        None,
        &mut recovered,
    )
    .map_err(|e| format!("TS parse error: {e:?}"))?;

    // 连「可恢复」的语法错误也当失败：带着错擦出来的代码跑不起来，不如原样
    // 报给用户。
    if !recovered.is_empty() {
        let msgs: Vec<String> = recovered.iter().map(|e| format!("{e:?}")).collect();
        return Err(format!("TS parse errors:\n{}", msgs.join("\n")));
    }

    // swc 的变换需要 `Globals` 上下文：resolver 先给标识符打标记，strip 才能
    // 正确删掉类型引用，最后 fixer 收拾残留的括号/分号。
    let output = GLOBALS.set(&Globals::default(), || {
        let unresolved = Mark::new();
        let top_level = Mark::new();
        program
            .apply(resolver(unresolved, top_level, true))
            .apply(strip(unresolved, top_level))
            .apply(fixer(None))
    });

    Ok(to_code_default(cm, None, &output))
}

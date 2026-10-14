//! TypeScript → JavaScript type stripping.
//!
//! Uses swc to parse TypeScript, strip type annotations, and emit JavaScript.
//! `import` statements are preserved so rquickjs's module loader can resolve
//! them at runtime. No ES version downleveling is performed.

use swc_common::{FileName, GLOBALS, Globals, Mark, SourceMap, sync::Lrc};
use swc_ecma_ast::{EsVersion, Program};
use swc_ecma_codegen::to_code_default;
use swc_ecma_parser::{Syntax, TsSyntax, parse_file_as_program};
use swc_ecma_transforms_base::{fixer::fixer, resolver};
use swc_ecma_transforms_typescript::strip;

/// Strip TypeScript type annotations from `ts_source`, returning valid JS
/// source with `import`/`export` statements intact.
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

    if !recovered.is_empty() {
        let msgs: Vec<String> = recovered.iter().map(|e| format!("{e:?}")).collect();
        return Err(format!("TS parse errors:\n{}", msgs.join("\n")));
    }

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

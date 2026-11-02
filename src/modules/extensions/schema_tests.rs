//! 清单 schema 与真实清单的一致性测试。
//!
//! `schemas/extension.schema.json` 是**手写**的（编译期嵌进
//! [`EXTENSION_SCHEMA`]）：它既给编辑器做补全，也等于一份「清单说明书」。
//! 手写的东西会漂移，所以这里用 `bolt_extensions::schema::problems` 反查三件事：
//!
//! 1. 脚手架生成的每种模板都符合 schema；
//! 2. 仓库里的示例扩展都符合 schema（文档里的写法必须本身合法）；
//! 3. 校验器真能挑出错误（否则前两条只是空转）。

use std::path::Path;

use super::manifest::{DEMO_MANIFEST, EXTENSION_SCHEMA, MANIFEST_FILE};
use super::scaffold::{ExtensionSpec, Template, files};
use bolt_extensions::schema::problems;

fn schema() -> serde_json::Value {
    serde_json::from_str(EXTENSION_SCHEMA).expect("嵌入的 schema 必须是合法 JSON")
}

fn check(label: &str, content: &str) -> serde_json::Value {
    let value: serde_json::Value =
        serde_json::from_str(content).unwrap_or_else(|e| panic!("{label}: 不是合法 JSON：{e}"));
    let problems = problems(&schema(), &value);
    assert!(problems.is_empty(), "{label}: {problems:#?}");
    value
}

fn spec(template: Template) -> ExtensionSpec {
    ExtensionSpec {
        id: "demo".to_string(),
        name: "演示".to_string(),
        version: "1.0.0".to_string(),
        template,
        permissions: vec!["env".to_string()],
        clipboard: true,
    }
}

fn manifest_of(spec: &ExtensionSpec) -> String {
    files(spec)
        .into_iter()
        .find(|(name, _)| *name == MANIFEST_FILE)
        .map(|(_, content)| content)
        .expect("每种模板都该有清单")
}

#[test]
fn scaffolded_manifests_match_the_schema() {
    for template in Template::ALL {
        let content = manifest_of(&spec(template));
        let value = check(&format!("{template:?} 模板"), &content);
        // schema 摆在扩展根目录，扩展目录相对它是一层上：两边必须写同一个字符串，
        // 否则「新建扩展」出来的清单第一行就是死链。
        assert_eq!(
            value["$schema"], "../extension.schema.json",
            "{template:?} 模板的 $schema 与实际落盘位置不一致"
        );
    }
}

#[test]
fn demo_manifest_matches_the_schema() {
    check("首启示例扩展", DEMO_MANIFEST);
}

#[test]
fn bundled_examples_match_the_schema() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions");
    let mut checked = 0;
    for entry in std::fs::read_dir(&root)
        .expect("仓库里的 extensions/ 应该存在")
        .flatten()
    {
        let manifest = entry.path().join(MANIFEST_FILE);
        if !manifest.is_file() {
            continue;
        }
        let name = entry.file_name();
        check(
            &format!("extensions/{}", name.to_string_lossy()),
            &std::fs::read_to_string(&manifest).expect("read manifest"),
        );
        checked += 1;
    }
    assert!(checked >= 2, "示例扩展不该被删到只剩 {checked} 个");
}

#[test]
fn the_checker_rejects_what_the_parser_rejects() {
    // 校验器自己得有效：拿两条真实会失败的清单试它，否则上面几条测试只是把
    // 任何输入都放过。
    let both = r#"{ "commands": [ { "id": "a", "title": "A", "ui": {
        "actions": [ { "builtin": "reload", "command": "a" } ] } } ] }"#;
    let found = problems(&schema(), &serde_json::from_str(both).expect("json"));
    assert!(
        found.iter().any(|problem| problem.contains("oneOf")),
        "builtin 与 command 同给时必须报错：{found:#?}"
    );

    let typo = r#"{ "commands": [ { "id": "a", "title": "A", "ui": { "alwaysOnTop": true } } ] }"#;
    let found = problems(&schema(), &serde_json::from_str(typo).expect("json"));
    assert!(
        found.iter().any(|problem| problem.contains("alwaysOnTop")),
        "拼错的字段名必须报错：{found:#?}"
    );
}

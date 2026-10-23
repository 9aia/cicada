//! Golden: the `tools/list` surface — names, descriptions, input
//! schemas, read-only hints — frozen against the committed fixture.
//! Regenerate deliberately with `CICADA_BLESS=1`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

#[test]
fn tools_list_is_golden() {
    let tools: Vec<serde_json::Value> = cicada_mcp::tools::definitions()
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
                "readOnlyHint": t.read_only,
            })
        })
        .collect();
    let actual = format!("{}\n", serde_json::to_string_pretty(&tools).unwrap());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tools-list.golden.json");
    if std::env::var_os("CICADA_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).expect("bless fixture");
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read golden {}: {e} — create it with CICADA_BLESS=1",
            path.display()
        )
    });
    assert_eq!(actual, expected, "tools/list golden drifted");
}

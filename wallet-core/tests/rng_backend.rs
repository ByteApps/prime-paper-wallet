//! Structural contract for the RNG backend: assert the generator we THINK
//! we linked is the one we linked.
//!
//! Statistics can never catch disclosure bug 1 (a fixed-seed CSPRNG is
//! statistically perfect — see `entropy.rs`'s
//! `control_fixed_seed_passes_and_that_is_the_point`). The only defense is
//! structural: pin the source files and the dependency graph so a future
//! edit that quietly rebinds `getrandom` to something else fails a build
//! or a test, rather than shipping a predictable key generator.
//!
//! Every contract below is written as a pure function over text/JSON, with
//! a thin wrapper that feeds it the real repo files — so the "what if this
//! regresses" cases are permanent unit tests (mutating a real file on disk
//! to prove a check can fail would be destructive and racy under `cargo
//! test`'s parallel harness), not one-off manual edits.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;

// =====================================================================
// Paths to the real files, resolved at compile time so they are correct
// regardless of the process's current working directory.
// =====================================================================

fn workspace_root() -> PathBuf {
    // Canonicalized so every path built from it (and every path the
    // directory walk below discovers) is a clean absolute path with no
    // `..` segments — otherwise two paths to the same file built via
    // different join sequences compare unequal under `Path`'s
    // component-wise `PartialEq` (it does not resolve `..`).
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("workspace root must exist")
}

fn root_cargo_toml() -> PathBuf {
    workspace_root().join("Cargo.toml")
}

fn getrandom_lib_rs() -> PathBuf {
    workspace_root().join("vendor/getrandom/src/lib.rs")
}

fn getrandom_xous_rs() -> PathBuf {
    workspace_root().join("vendor/getrandom/src/xous.rs")
}

fn getrandom_vendor_dir() -> PathBuf {
    workspace_root().join("vendor/getrandom")
}

// =====================================================================
// Contract 1 — [patch.crates-io] redirects getrandom to the vendored copy
// =====================================================================

/// Does `[patch.crates-io]` in `cargo_toml` redirect `getrandom` to a
/// `vendor/getrandom` path? Pure text scan, deliberately not a full TOML
/// parser: this file has no `toml` crate dependency, and the shape we're
/// pinning is narrow enough that a substring check over the section body
/// is both sufficient and easy to mutation-test.
fn patches_getrandom_to_vendor(cargo_toml: &str) -> bool {
    let Some(section) = extract_toml_section(cargo_toml, "patch.crates-io") else {
        return false;
    };
    section.lines().any(|line| {
        let line = line.trim();
        line.starts_with("getrandom") && line.contains("path") && line.contains("vendor/getrandom")
    })
}

/// The body of a top-level `[section]`: everything up to (not including)
/// the next line that starts a new top-level `[...]` table, or EOF.
fn extract_toml_section<'a>(text: &'a str, section: &str) -> Option<&'a str> {
    let header = format!("[{section}]");
    let start = text.find(&header)? + header.len();
    let rest = &text[start..];
    let end = rest.find("\n[").unwrap_or(rest.len());
    Some(&rest[..end])
}

#[test]
fn contract_1_real_cargo_toml_patches_getrandom() {
    let text = std::fs::read_to_string(root_cargo_toml()).expect("read root Cargo.toml");
    assert!(
        patches_getrandom_to_vendor(&text),
        "root Cargo.toml no longer redirects getrandom to vendor/getrandom via [patch.crates-io]"
    );
}

#[test]
fn contract_1_mutation_no_patch_section_fails() {
    let text = "[package]\nname = \"x\"\n\n[dependencies]\ngetrandom = \"0.2\"\n";
    assert!(!patches_getrandom_to_vendor(text), "no [patch.crates-io] section at all must fail");
}

#[test]
fn contract_1_mutation_patch_section_for_other_crate_fails() {
    let text = "[patch.crates-io]\nsome-other-crate = { path = \"vendor/other\" }\n";
    assert!(!patches_getrandom_to_vendor(text), "a patch section that never mentions getrandom must fail");
}

#[test]
fn contract_1_mutation_getrandom_patched_elsewhere_fails() {
    let text = "[patch.crates-io]\ngetrandom = { path = \"third_party/not-vendored\" }\n";
    assert!(!patches_getrandom_to_vendor(text), "getrandom patched to a non-vendor path must fail");
}

#[test]
fn contract_1_mutation_getrandom_patched_via_git_fails() {
    let text = "[patch.crates-io]\ngetrandom = { git = \"https://example.com/getrandom\" }\n";
    assert!(!patches_getrandom_to_vendor(text), "a git-sourced patch (no local path) must fail");
}

#[test]
fn contract_1_mutation_commented_out_patch_fails() {
    let text = "[patch.crates-io]\n# getrandom = { path = \"vendor/getrandom\" }\n";
    assert!(!patches_getrandom_to_vendor(text), "a commented-out patch line must not count");
}

#[test]
fn contract_1_positive_synthetic_patch_passes() {
    let text = "[package]\nname = \"x\"\n\n[patch.crates-io]\ngetrandom = { path = \"vendor/getrandom\" }\n\n[dependencies]\nfoo = \"1\"\n";
    assert!(patches_getrandom_to_vendor(text), "a well-formed patch section must pass");
}

// =====================================================================
// Contract 2 — cfg_if ordering in vendor/getrandom/src/lib.rs:
//   * a `#[cfg(keyos)]` arm exists
//   * it appears BEFORE the `feature = "custom"` arm
//   * the final fallback arm is `compile_error!`
// =====================================================================

#[derive(Debug)]
enum CfgIfViolation {
    NoKeyosArm,
    NoCustomFeatureArm,
    KeyosAfterCustom { keyos_pos: usize, custom_pos: usize },
    NoBareElseArm,
    FinalArmNotCompileError,
}

impl std::fmt::Display for CfgIfViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoKeyosArm => write!(f, "no `cfg(keyos)` arm found in the cfg_if! chain"),
            Self::NoCustomFeatureArm => write!(f, "no `feature = \"custom\"` arm found in the cfg_if! chain"),
            Self::KeyosAfterCustom { keyos_pos, custom_pos } => write!(
                f,
                "cfg(keyos) arm (byte {keyos_pos}) must come BEFORE the custom-feature arm \
                 (byte {custom_pos}) — otherwise an unflagged device build could silently \
                 rebind to the custom backend instead of failing to build"
            ),
            Self::NoBareElseArm => write!(f, "no final bare `else {{` fallback arm found in the cfg_if! chain"),
            Self::FinalArmNotCompileError => {
                write!(f, "the final fallback arm does not contain `compile_error!`")
            }
        }
    }
}

/// Extract the body of the (first) `cfg_if! { ... }` invocation in `text`,
/// via brace balancing from the opening `{` after `cfg_if!`.
fn extract_cfg_if_block(text: &str) -> Option<&str> {
    let marker = "cfg_if!";
    let marker_pos = text.find(marker)?;
    let open_rel = text[marker_pos..].find('{')?;
    let open = marker_pos + open_rel;
    let mut depth = 0i32;
    for (i, ch) in text[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    // body is strictly between the outer braces
                    return Some(&text[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Byte offset of the body of the LAST bare `else { ... }` arm (as opposed
/// to an `else if ...`) within `block`. Simple substring scan, not a real
/// parser — adequate for pinning this file's specific shape, and every
/// case is covered by a mutation test below.
fn last_bare_else_body(block: &str) -> Option<&str> {
    let mut idx = 0usize;
    let mut best: Option<usize> = None;
    while let Some(rel) = block[idx..].find("else") {
        let else_start = idx + rel;
        let after = else_start + "else".len();
        let trimmed = block[after..].trim_start();
        if trimmed.starts_with("if") {
            // an `else if` arm — not a candidate
        } else if let Some(stripped) = trimmed.strip_prefix('{') {
            let body_start = block.len() - stripped.len();
            best = Some(body_start);
        }
        idx = after;
    }
    best.map(|start| &block[start..])
}

fn check_cfg_if_chain(lib_rs: &str) -> Result<(), CfgIfViolation> {
    let block = extract_cfg_if_block(lib_rs).ok_or(CfgIfViolation::NoKeyosArm)?;
    let keyos_pos = block.find("cfg(keyos)").ok_or(CfgIfViolation::NoKeyosArm)?;
    let custom_pos = block.find("feature = \"custom\"").ok_or(CfgIfViolation::NoCustomFeatureArm)?;
    if keyos_pos >= custom_pos {
        return Err(CfgIfViolation::KeyosAfterCustom { keyos_pos, custom_pos });
    }
    let final_arm = last_bare_else_body(block).ok_or(CfgIfViolation::NoBareElseArm)?;
    if !final_arm.contains("compile_error!") {
        return Err(CfgIfViolation::FinalArmNotCompileError);
    }
    Ok(())
}

#[test]
fn contract_2_real_lib_rs_cfg_if_chain_is_ordered_correctly() {
    let text = std::fs::read_to_string(getrandom_lib_rs()).expect("read vendor/getrandom/src/lib.rs");
    check_cfg_if_chain(&text).unwrap_or_else(|e| panic!("{e}"));
}

const SYNTH_GOOD: &str = r#"
cfg_if! {
    if #[cfg(windows)] {
        mod windows;
    } else if #[cfg(keyos)] {
        mod xous;
    } else if #[cfg(feature = "custom")] {
        use custom as imp;
    } else {
        compile_error!("target is not supported");
    }
}
"#;

#[test]
fn contract_2_positive_synthetic_good_chain_passes() {
    check_cfg_if_chain(SYNTH_GOOD).expect("well-ordered synthetic chain must pass");
}

#[test]
fn contract_2_mutation_custom_before_keyos_fails() {
    let text = r#"
cfg_if! {
    if #[cfg(feature = "custom")] {
        use custom as imp;
    } else if #[cfg(keyos)] {
        mod xous;
    } else {
        compile_error!("target is not supported");
    }
}
"#;
    let err = check_cfg_if_chain(text).expect_err("custom-before-keyos ordering must fail");
    assert!(matches!(err, CfgIfViolation::KeyosAfterCustom { .. }), "wrong violation: {err}");
}

#[test]
fn contract_2_mutation_missing_keyos_arm_fails() {
    let text = r#"
cfg_if! {
    if #[cfg(feature = "custom")] {
        use custom as imp;
    } else {
        compile_error!("target is not supported");
    }
}
"#;
    let err = check_cfg_if_chain(text).expect_err("missing cfg(keyos) arm must fail");
    assert!(matches!(err, CfgIfViolation::NoKeyosArm), "wrong violation: {err}");
}

#[test]
fn contract_2_mutation_missing_custom_arm_fails() {
    let text = r#"
cfg_if! {
    if #[cfg(keyos)] {
        mod xous;
    } else {
        compile_error!("target is not supported");
    }
}
"#;
    let err = check_cfg_if_chain(text).expect_err("missing feature=\"custom\" arm must fail");
    assert!(matches!(err, CfgIfViolation::NoCustomFeatureArm), "wrong violation: {err}");
}

#[test]
fn contract_2_mutation_final_arm_not_compile_error_fails() {
    let text = r#"
cfg_if! {
    if #[cfg(keyos)] {
        mod xous;
    } else if #[cfg(feature = "custom")] {
        use custom as imp;
    } else {
        use fallback_rng as imp;
    }
}
"#;
    let err = check_cfg_if_chain(text).expect_err("a silent fallback instead of compile_error! must fail");
    assert!(matches!(err, CfgIfViolation::FinalArmNotCompileError), "wrong violation: {err}");
}

#[test]
fn contract_2_mutation_no_bare_else_fallback_fails() {
    // Every arm is `else if` — no unconditional fallback at all, i.e. an
    // unsupported target would compile with NO getrandom backend selected
    // (a link error downstream) instead of a clear compile_error!.
    let text = r#"
cfg_if! {
    if #[cfg(keyos)] {
        mod xous;
    } else if #[cfg(feature = "custom")] {
        use custom as imp;
    }
}
"#;
    let err = check_cfg_if_chain(text).expect_err("chain with no bare-else fallback must fail");
    assert!(matches!(err, CfgIfViolation::NoBareElseArm), "wrong violation: {err}");
}

// =====================================================================
// Contract 3 — xous.rs still calls the fill-verification hardening
// =====================================================================

fn calls_fill_verification(xous_rs: &str) -> Result<(), Vec<&'static str>> {
    let required = ["write_sentinel(", "looks_unfilled(", "words_for("];
    let missing: Vec<&'static str> = required.into_iter().filter(|f| !xous_rs.contains(f)).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

#[test]
fn contract_3_real_xous_rs_calls_fill_verification() {
    let text = std::fs::read_to_string(getrandom_xous_rs()).expect("read vendor/getrandom/src/xous.rs");
    calls_fill_verification(&text).unwrap_or_else(|missing| {
        panic!("xous.rs no longer calls: {missing:?} — the fill-verification hardening may have been reverted")
    });
}

#[test]
fn contract_3_positive_synthetic_calls_pass() {
    let text = "fn f() { write_sentinel(&mut buf); let ok = !looks_unfilled(&buf); let n = words_for(len); }";
    assert!(calls_fill_verification(text).is_ok());
}

#[test]
fn contract_3_mutation_missing_write_sentinel_fails() {
    let text = "fn f() { let ok = !looks_unfilled(&buf); let n = words_for(len); }";
    let missing = calls_fill_verification(text).unwrap_err();
    assert_eq!(missing, vec!["write_sentinel("]);
}

#[test]
fn contract_3_mutation_missing_looks_unfilled_fails() {
    let text = "fn f() { write_sentinel(&mut buf); let n = words_for(len); }";
    let missing = calls_fill_verification(text).unwrap_err();
    assert_eq!(missing, vec!["looks_unfilled("]);
}

#[test]
fn contract_3_mutation_missing_words_for_fails() {
    let text = "fn f() { write_sentinel(&mut buf); let ok = !looks_unfilled(&buf); }";
    let missing = calls_fill_verification(text).unwrap_err();
    assert_eq!(missing, vec!["words_for("]);
}

#[test]
fn contract_3_mutation_all_three_missing_fails() {
    let text = "fn f() { /* hardening reverted */ }";
    let missing = calls_fill_verification(text).unwrap_err();
    assert_eq!(missing, vec!["write_sentinel(", "looks_unfilled(", "words_for("]);
}

// =====================================================================
// Contract 4 — register_custom_getrandom! appears nowhere outside
// vendor/getrandom
// =====================================================================

const FORBIDDEN_MACRO: &str = "register_custom_getrandom!";

/// Given a set of `(relative_path, content)` pairs already filtered to
/// exclude the `vendor/getrandom` subtree, return every path that still
/// mentions the custom-RNG registration macro.
fn files_referencing_forbidden_macro<'a>(files: &[(&'a str, &'a str)]) -> Vec<&'a str> {
    files.iter().filter(|(_, content)| content.contains(FORBIDDEN_MACRO)).map(|(path, _)| *path).collect()
}

#[test]
fn contract_4_positive_synthetic_clean_tree_passes() {
    let files = [("src/main.rs", "fn main() {}"), ("src/lib.rs", "pub fn f() {}")];
    assert!(files_referencing_forbidden_macro(&files).is_empty());
}

#[test]
fn contract_4_mutation_reference_outside_vendor_is_detected() {
    let files = [("src/main.rs", "fn main() {}"), ("src/evil.rs", "register_custom_getrandom!(always_fail);")];
    let hits = files_referencing_forbidden_macro(&files);
    assert_eq!(hits, vec!["src/evil.rs"]);
}

#[test]
fn contract_4_mutation_multiple_references_all_detected() {
    let files = [
        ("src/a.rs", "register_custom_getrandom!(f);"),
        ("src/b.rs", "// see register_custom_getrandom! docs"),
        ("src/c.rs", "fn clean() {}"),
    ];
    let hits = files_referencing_forbidden_macro(&files);
    assert_eq!(hits, vec!["src/a.rs", "src/b.rs"]);
}

/// Recursively collect `(path, content)` for every `.rs` file under `root`,
/// skipping `target`, `.git`, and — for THIS specific walk — the
/// `vendor/getrandom` subtree itself (which legitimately defines and
/// documents the macro). Symlinks are not followed, to stay well clear of
/// the workspace-level `keyos-sdk` symlink convention noted in the repo's
/// own CLAUDE.md.
fn collect_rs_files_excluding(root: &Path, exclude: &[&Path]) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue }; // metadata() does not follow symlinks on DirEntry
            if meta.is_symlink() {
                continue;
            }
            if exclude.iter().any(|ex| path == *ex) {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if meta.is_dir() {
                if name == "target" || name == ".git" {
                    continue;
                }
                stack.push(path);
            } else if name.ends_with(".rs") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    out.push((path, content));
                }
            }
        }
    }
    out
}

#[test]
fn contract_4_real_tree_has_no_registration_outside_vendor() {
    let root = workspace_root();
    // Exclude the vendored crate itself (it legitimately defines and
    // documents the macro) AND this test file (which legitimately quotes
    // the macro's NAME as a string constant to detect it elsewhere —
    // that quoting is not an invocation).
    let this_file = root.join("wallet-core/tests/rng_backend.rs");
    let exclude = [getrandom_vendor_dir(), this_file];
    let exclude_refs: Vec<&Path> = exclude.iter().map(|p| p.as_path()).collect();
    let files = collect_rs_files_excluding(&root, &exclude_refs);
    assert!(!files.is_empty(), "sanity: the walk found no .rs files at all — path resolution is broken");

    let refs: Vec<&Path> = files.iter().filter(|(_, c)| c.contains(FORBIDDEN_MACRO)).map(|(p, _)| p.as_path()).collect();
    assert!(
        refs.is_empty(),
        "{FORBIDDEN_MACRO} referenced outside vendor/getrandom: {refs:?} — a custom RNG backend \
         could silently take priority on an unsupported-target build"
    );
}

// =====================================================================
// Contract 5 — dependency-graph guard: the ONLY `getrandom` reachable
// through normal (non-dev) dependencies from the app root, on the device
// target, is the vendored one.
// =====================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
struct GetrandomPkg {
    id: String,
    source: Option<String>,
    manifest_path: String,
}

/// Walk `metadata["resolve"]["nodes"]` from `root`, following only edges
/// whose `dep_kinds` include the normal (`kind: null`) or `"build"` kind.
/// Returns every package id reached this way (not filtered to
/// `getrandom` — kept generic so the graph-walk itself is independently
/// mutation-testable from the getrandom-specific interpretation below).
fn reachable_via_normal_or_build(metadata: &Value, root: &str) -> BTreeSet<String> {
    let empty_nodes: Vec<Value> = Vec::new();
    let nodes = metadata["resolve"]["nodes"].as_array().unwrap_or(&empty_nodes);
    let by_id: HashMap<&str, &Value> = nodes.iter().filter_map(|n| n["id"].as_str().map(|id| (id, n))).collect();

    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![root.to_string()];
    let empty_deps: Vec<Value> = Vec::new();
    while let Some(pid) = stack.pop() {
        if !visited.insert(pid.clone()) {
            continue;
        }
        let Some(node) = by_id.get(pid.as_str()) else { continue };
        let deps = node["deps"].as_array().unwrap_or(&empty_deps);
        for dep in deps {
            let empty_kinds: Vec<Value> = Vec::new();
            let kinds = dep["dep_kinds"].as_array().unwrap_or(&empty_kinds);
            let is_normal_or_build = kinds.iter().any(|k| {
                let kind = k["kind"].as_str();
                kind.is_none() || kind == Some("build")
            });
            if !is_normal_or_build {
                continue;
            }
            if let Some(target) = dep["pkg"].as_str() {
                if !visited.contains(target) {
                    stack.push(target.to_string());
                }
            }
        }
    }
    visited
}

/// Every `getrandom`-named package declared in `metadata["packages"]`.
fn getrandom_packages(metadata: &Value) -> Vec<GetrandomPkg> {
    let empty: Vec<Value> = Vec::new();
    metadata["packages"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter(|p| p["name"].as_str() == Some("getrandom"))
        .map(|p| GetrandomPkg {
            id: p["id"].as_str().unwrap_or_default().to_string(),
            source: p["source"].as_str().map(str::to_string),
            manifest_path: p["manifest_path"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

/// The `getrandom` packages that are BOTH declared AND reachable through
/// normal/build edges from `root`.
fn reachable_getrandom(metadata: &Value, root: &str) -> Vec<GetrandomPkg> {
    let reachable = reachable_via_normal_or_build(metadata, root);
    getrandom_packages(metadata).into_iter().filter(|p| reachable.contains(&p.id)).collect()
}

fn check_single_vendored_getrandom(hits: &[GetrandomPkg]) -> Result<(), String> {
    if hits.len() != 1 {
        return Err(format!(
            "expected exactly 1 reachable `getrandom`, found {}: {:?}",
            hits.len(),
            hits.iter().map(|h| &h.id).collect::<Vec<_>>()
        ));
    }
    let hit = &hits[0];
    if hit.source.is_some() {
        return Err(format!(
            "the one reachable getrandom is not a local path dependency (source={:?}) — id={}",
            hit.source, hit.id
        ));
    }
    if !hit.manifest_path.ends_with("vendor/getrandom/Cargo.toml") {
        return Err(format!("the one reachable getrandom is not the vendored copy: {}", hit.manifest_path));
    }
    Ok(())
}

// ---- unit tests over synthetic metadata JSON (mutation coverage) ----

fn synth_metadata(root_deps: &[(&str, &str)], extra_nodes: &[(&str, &[(&str, &str)])], packages: &[(&str, &str, Option<&str>, &str)]) -> (Value, String) {
    // root_deps / extra_nodes: (target_id, kind) pairs per node ("kind" is
    // "normal", "build", or "dev").
    let root = "root#0.1.0".to_string();
    let mk_deps = |deps: &[(&str, &str)]| -> Value {
        Value::Array(
            deps.iter()
                .map(|(target, kind)| {
                    let kind_val = if *kind == "normal" { Value::Null } else { Value::String((*kind).to_string()) };
                    serde_json::json!({ "pkg": target, "dep_kinds": [{ "kind": kind_val, "target": null }] })
                })
                .collect(),
        )
    };
    let mut nodes = vec![serde_json::json!({ "id": root, "deps": mk_deps(root_deps) })];
    for (id, deps) in extra_nodes {
        nodes.push(serde_json::json!({ "id": id, "deps": mk_deps(deps) }));
    }
    let pkgs: Vec<Value> = packages
        .iter()
        .map(|(id, name, source, manifest_path)| {
            serde_json::json!({ "id": id, "name": name, "source": source, "manifest_path": manifest_path })
        })
        .collect();
    let metadata = serde_json::json!({
        "resolve": { "root": root, "nodes": nodes },
        "packages": pkgs,
    });
    (metadata, root)
}

#[test]
fn contract_5_reachable_vendored_only_passes() {
    let (metadata, root) = synth_metadata(
        &[("vendor#getrandom@0.2.10", "normal")],
        &[("vendor#getrandom@0.2.10", &[])],
        &[("vendor#getrandom@0.2.10", "getrandom", None, "/repo/vendor/getrandom/Cargo.toml")],
    );
    let hits = reachable_getrandom(&metadata, &root);
    check_single_vendored_getrandom(&hits).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn contract_5_mutation_dev_only_edge_is_not_reachable() {
    // The vendored copy is reachable normally; a second, "evil" getrandom
    // is declared but only ever reached via a dev-dependency edge — it
    // must NOT count, and the vendored one alone must still pass.
    let (metadata, root) = synth_metadata(
        &[("vendor#getrandom@0.2.10", "normal"), ("evil#getrandom@0.3.4", "dev")],
        &[("vendor#getrandom@0.2.10", &[]), ("evil#getrandom@0.3.4", &[])],
        &[
            ("vendor#getrandom@0.2.10", "getrandom", None, "/repo/vendor/getrandom/Cargo.toml"),
            ("evil#getrandom@0.3.4", "getrandom", Some("registry+https://crates.io"), "/cargo/registry/getrandom-0.3.4/Cargo.toml"),
        ],
    );
    let hits = reachable_getrandom(&metadata, &root);
    assert_eq!(hits.len(), 1, "a dev-only edge must not make evil-getrandom reachable: {hits:?}");
    check_single_vendored_getrandom(&hits).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn contract_5_mutation_unreachable_node_is_excluded() {
    // "evil" getrandom exists in `packages` (e.g. present in Cargo.lock)
    // but has no edge from root at all.
    let (metadata, root) = synth_metadata(
        &[("vendor#getrandom@0.2.10", "normal")],
        &[("vendor#getrandom@0.2.10", &[])],
        &[
            ("vendor#getrandom@0.2.10", "getrandom", None, "/repo/vendor/getrandom/Cargo.toml"),
            ("evil#getrandom@0.4.2", "getrandom", Some("registry+https://crates.io"), "/cargo/registry/getrandom-0.4.2/Cargo.toml"),
        ],
    );
    let hits = reachable_getrandom(&metadata, &root);
    assert_eq!(hits.len(), 1);
    check_single_vendored_getrandom(&hits).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn contract_5_mutation_second_normal_getrandom_is_flagged() {
    // The real regression this guards against: something in the normal
    // dependency graph (e.g. a bumped `rand 0.9` user) pulls in a SECOND
    // getrandom that is actually reachable on the device build. Must FAIL.
    let (metadata, root) = synth_metadata(
        &[("vendor#getrandom@0.2.10", "normal"), ("rand#0.9.2", "normal")],
        &[("vendor#getrandom@0.2.10", &[]), ("rand#0.9.2", &[("rand_core#0.9.5", "normal")]), ("rand_core#0.9.5", &[("bypass#getrandom@0.3.4", "normal")]), ("bypass#getrandom@0.3.4", &[])],
        &[
            ("vendor#getrandom@0.2.10", "getrandom", None, "/repo/vendor/getrandom/Cargo.toml"),
            ("bypass#getrandom@0.3.4", "getrandom", Some("registry+https://crates.io"), "/cargo/registry/getrandom-0.3.4/Cargo.toml"),
        ],
    );
    let hits = reachable_getrandom(&metadata, &root);
    assert_eq!(hits.len(), 2, "expected both getrandoms reachable in this synthetic bypass graph: {hits:?}");
    let err = check_single_vendored_getrandom(&hits).expect_err("two reachable getrandoms must be flagged");
    assert!(err.contains("found 2"), "error should report the count: {err}");
}

#[test]
fn contract_5_mutation_build_kind_edge_is_reachable() {
    // A getrandom pulled in only via a BUILD dependency of the root must
    // still be caught — the brief explicitly includes "build" alongside
    // normal. Here it's the ONLY getrandom (vendored one omitted from the
    // graph on purpose) so a correct implementation must still find it
    // and therefore correctly flag it as non-vendored.
    let (metadata, root) = synth_metadata(
        &[("buildtool#1.0.0", "build")],
        &[("buildtool#1.0.0", &[("via_build#getrandom@0.3.4", "normal")]), ("via_build#getrandom@0.3.4", &[])],
        &[("via_build#getrandom@0.3.4", "getrandom", Some("registry+https://crates.io"), "/cargo/registry/getrandom-0.3.4/Cargo.toml")],
    );
    let hits = reachable_getrandom(&metadata, &root);
    assert_eq!(hits.len(), 1, "a getrandom reached only through a build-dependency chain must be found: {hits:?}");
    let err = check_single_vendored_getrandom(&hits).expect_err("the reached copy is not vendored, must fail");
    assert!(err.contains("not a local path dependency") || err.contains("not the vendored copy"), "{err}");
}

#[test]
fn contract_5_mutation_dual_kind_edge_counts_as_reachable() {
    // cargo metadata can report a single edge used both ways (dep_kinds
    // has BOTH a "dev" entry and a normal entry) when the target crate is
    // both a normal and dev dependency elsewhere in the graph. Any match
    // must count.
    let root = "root#0.1.0".to_string();
    let metadata = serde_json::json!({
        "resolve": {
            "root": root,
            "nodes": [
                { "id": root, "deps": [
                    { "pkg": "vendor#getrandom@0.2.10", "dep_kinds": [ { "kind": "dev", "target": null }, { "kind": null, "target": null } ] }
                ] },
                { "id": "vendor#getrandom@0.2.10", "deps": [] }
            ]
        },
        "packages": [
            { "id": "vendor#getrandom@0.2.10", "name": "getrandom", "source": null, "manifest_path": "/repo/vendor/getrandom/Cargo.toml" }
        ]
    });
    let hits = reachable_getrandom(&metadata, &root);
    assert_eq!(hits.len(), 1, "an edge with BOTH a dev and a normal kind entry must still be reachable");
}

// ---- wrapper: run the REAL cargo metadata against the REAL device target ----

/// Locate a `nix` executable without assuming the ambient shell has already
/// sourced the multi-user daemon's profile script (workspace CLAUDE.md,
/// "Environment / toolchain": a non-login shell needs
/// `. '/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh'` first).
/// Tries PATH, then the standard multi-user install location.
fn nix_binary() -> String {
    if std::process::Command::new("nix").arg("--version").output().is_ok_and(|o| o.status.success()) {
        return "nix".to_string();
    }
    const FALLBACK: &str = "/nix/var/nix/profiles/default/bin/nix";
    if Path::new(FALLBACK).exists() {
        return FALLBACK.to_string();
    }
    panic!(
        "no `nix` executable found on PATH or at {FALLBACK} — the device-target dependency \
         graph check needs the Foundation SDK's Nix shell, which needs Nix itself"
    );
}

#[test]
fn contract_5_real_graph_reaches_only_the_vendored_getrandom_on_device() {
    let manifest = root_cargo_toml();
    // `--filter-platform` makes cargo ask rustc to evaluate this target's
    // `cfg()`s. `armv7a-unknown-xous-elf` is a KeyOS target patched into the
    // Foundation SDK's Nix-provided nightly rustc (`foundation doctor`'s
    // "KeyOS target" check); the standalone rustup toolchain plain `cargo
    // test -p wallet-core` runs under has no idea it exists at all
    // (`rustc --print target-list` has no such entry there). Even inside
    // the SDK's Nix shell, resolving this target additionally requires
    // `-Zunstable-options` (that nightly rustc treats it as a "custom"
    // target) — verified against `foundation`'s own embedded RUSTFLAGS for
    // real hardware builds, which carries the same flag. So this one
    // metadata call is routed through `nix develop <sdk root> --command
    // cargo metadata ...` with `-Zunstable-options` scoped to its own
    // RUSTFLAGS, rather than exporting that flag for real builds (which
    // `foundation` already does on its own).
    // FOUNDATION_SDK_ROOT is only trusted when it actually looks like the SDK
    // checkout (i.e. it has a flake.nix). Running this test the way the app's
    // CLAUDE.md documents — `nix develop <sdk> --command cargo test` — puts us
    // INSIDE the SDK shell, which exports FOUNDATION_SDK_ROOT pointing at the
    // current PROJECT, not the SDK. Trusting it blindly then shells into a
    // directory with no flake and the check fails with "is not part of a
    // flake", which looks like a broken guard rather than a bad path
    // (2026-08-27).
    let sdk_root = std::env::var("FOUNDATION_SDK_ROOT")
        .ok()
        .filter(|p| std::path::Path::new(p).join("flake.nix").exists())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            format!("{home}/.foundation/sdk/current")
        });
    let nix = nix_binary();
    let output = std::process::Command::new(&nix)
        .args([
            "develop",
            &sdk_root,
            "--command",
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            "armv7a-unknown-xous-elf",
            "--locked",
            "--manifest-path",
        ])
        .arg(&manifest)
        .env("RUSTFLAGS", "-Zunstable-options")
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "failed to run `{nix} develop {sdk_root} --command cargo metadata` for the device \
                 target: {e}\n\nThis check needs the Foundation SDK's Nix shell — run `foundation \
                 doctor` and make sure `nix develop {sdk_root}` works on its own first."
            )
        });

    assert!(
        output.status.success(),
        "`nix develop {sdk_root} --command cargo metadata --filter-platform \
         armv7a-unknown-xous-elf` failed (exit {:?}):\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The SDK's Nix flake prints a "Foundation SDK user shell ready."
    // banner via its shellHook, onto stdout, ahead of the real command's
    // own output — `nix develop --command` doesn't suppress it. The
    // metadata document itself always starts with `{`, so trim anything
    // the shell hook printed before it rather than fighting the hook.
    let json_start = stdout.find('{').expect("cargo metadata stdout contains no JSON object");
    let metadata: Value = serde_json::from_str(&stdout[json_start..]).expect("cargo metadata stdout must be valid JSON");
    let root = metadata["resolve"]["root"].as_str().expect("resolve.root must be a string").to_string();

    let hits = reachable_getrandom(&metadata, &root);
    check_single_vendored_getrandom(&hits).unwrap_or_else(|e| {
        panic!(
            "{e}\n\nfull reachable-getrandom set: {hits:#?}\n\n\
             This means the device build's dependency graph can reach a \
             getrandom OTHER than vendor/getrandom — the exact shape of a \
             `rand 0.9`-style bypass of the TRNG patch."
        )
    });
}

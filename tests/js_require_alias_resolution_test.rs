use tempfile::TempDir;
use tokensave::db::Database;
use tokensave::extraction::{LanguageExtractor, TypeScriptExtractor};
use tokensave::resolution::ReferenceResolver;
use tokensave::types::*;

async fn setup_db_with_nodes(nodes: &[Node]) -> (TempDir, Database) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let (db, _) = Database::initialize(&dir.path().join("test.db"))
        .await
        .expect("failed to init db");

    for node in nodes {
        db.insert_node(node).await.expect("failed to insert node");
    }
    (dir, db)
}

/// Fixture A (duplicate tree):
/// Main tree with `src/utils.js` (exports `fn`), `.github`-style nested consumer
/// requiring `../../../src/utils` and calling `lib.fn()`, and a root consumer;
/// plus an exact copy of all files under `worktree-x/`.
///
/// Asserts that call edges from each tree's consumer bind to its OWN tree's `utils.js`.
#[tokio::test]
async fn test_fixture_a_duplicate_tree_require_resolution() {
    let utils_code = r#"
function fn() {
    return 1;
}
module.exports = { fn };
"#;

    let nested_consumer_code = r#"
const lib = require('../../../src/utils');

function run() {
    return lib.fn();
}
"#;

    let root_consumer_code = r#"
const lib = require('./src/utils');

function main() {
    return lib.fn();
}
"#;

    let extractor = TypeScriptExtractor;

    let res_main_utils = extractor.extract("src/utils.js", utils_code);
    let res_main_nested =
        extractor.extract(".github/workflows/test/consumer.js", nested_consumer_code);
    let res_main_root = extractor.extract("root_consumer.js", root_consumer_code);

    let res_wt_utils = extractor.extract("worktree-x/src/utils.js", utils_code);
    let res_wt_nested = extractor.extract(
        "worktree-x/.github/workflows/test/consumer.js",
        nested_consumer_code,
    );
    let res_wt_root = extractor.extract("worktree-x/root_consumer.js", root_consumer_code);

    let mut all_nodes = Vec::new();
    let mut all_refs = Vec::new();

    for res in [
        &res_main_utils,
        &res_main_nested,
        &res_main_root,
        &res_wt_utils,
        &res_wt_nested,
        &res_wt_root,
    ] {
        all_nodes.extend(res.nodes.clone());
        all_refs.extend(res.unresolved_refs.clone());
    }

    let (_dir, db) = setup_db_with_nodes(&all_nodes).await;
    let resolver = ReferenceResolver::from_nodes(&db, &all_nodes);

    let main_fn_node = res_main_utils
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "fn")
        .expect("main fn node should exist");

    let wt_fn_node = res_wt_utils
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "fn")
        .expect("worktree fn node should exist");

    // 1. Resolve nested consumer in main tree
    let main_nested_ref = res_main_nested
        .unresolved_refs
        .iter()
        .find(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "lib.fn")
        .expect("main nested lib.fn ref should exist");
    let resolved_main_nested = resolver
        .resolve_one(main_nested_ref)
        .expect("main nested lib.fn should resolve");
    assert_eq!(
        resolved_main_nested.target_node_id, main_fn_node.id,
        "main nested consumer must bind to main tree's utils.js"
    );

    // 2. Resolve nested consumer in worktree-x
    let wt_nested_ref = res_wt_nested
        .unresolved_refs
        .iter()
        .find(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "lib.fn")
        .expect("worktree nested lib.fn ref should exist");
    let resolved_wt_nested = resolver
        .resolve_one(wt_nested_ref)
        .expect("worktree nested lib.fn should resolve");
    assert_eq!(
        resolved_wt_nested.target_node_id, wt_fn_node.id,
        "worktree nested consumer must bind to worktree tree's utils.js"
    );

    // 3. Resolve root consumer in main tree
    let main_root_ref = res_main_root
        .unresolved_refs
        .iter()
        .find(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "lib.fn")
        .expect("main root lib.fn ref should exist");
    let resolved_main_root = resolver
        .resolve_one(main_root_ref)
        .expect("main root lib.fn should resolve");
    assert_eq!(
        resolved_main_root.target_node_id, main_fn_node.id,
        "main root consumer must bind to main tree's utils.js"
    );

    // 4. Resolve root consumer in worktree-x
    let wt_root_ref = res_wt_root
        .unresolved_refs
        .iter()
        .find(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "lib.fn")
        .expect("worktree root lib.fn ref should exist");
    let resolved_wt_root = resolver
        .resolve_one(wt_root_ref)
        .expect("worktree root lib.fn should resolve");
    assert_eq!(
        resolved_wt_root.target_node_id, wt_fn_node.id,
        "worktree root consumer must bind to worktree tree's utils.js"
    );
}

/// Fixture B:
/// Alias known, relative specifier resolves to a file that lacks the symbol
/// -> assert NO edge is created (no global fallback to other files containing the symbol).
#[tokio::test]
async fn test_fixture_b_no_global_fallback_when_symbol_missing_in_target() {
    let other_code = r#"
function notFn() {
    return 2;
}
"#;

    let caller_code = r#"
const lib = require('./src/other');

function main() {
    return lib.fn();
}
"#;

    let unrelated_code = r#"
function fn() {
    return 1;
}
"#;

    let extractor = TypeScriptExtractor;

    let res_other = extractor.extract("src/other.js", other_code);
    let res_caller = extractor.extract("caller.js", caller_code);
    let res_unrelated = extractor.extract("some/unrelated/utils.js", unrelated_code);

    let mut all_nodes = Vec::new();
    let mut all_refs = Vec::new();

    for res in [&res_other, &res_caller, &res_unrelated] {
        all_nodes.extend(res.nodes.clone());
        all_refs.extend(res.unresolved_refs.clone());
    }

    let (_dir, db) = setup_db_with_nodes(&all_nodes).await;
    let resolver = ReferenceResolver::from_nodes(&db, &all_nodes);

    let call_ref = res_caller
        .unresolved_refs
        .iter()
        .find(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "lib.fn")
        .expect("caller lib.fn ref should exist");

    let resolved = resolver.resolve_one(call_ref);
    assert!(
        resolved.is_none(),
        "relative require alias pointing to file lacking symbol must NOT resolve via global fallback"
    );
}

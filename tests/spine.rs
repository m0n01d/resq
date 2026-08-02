//! Wave 1 (agent A1) — tests for the shared type spine: `lib.rs`, `parser.rs`, `writer.rs`.
//!
//! The load-bearing case is `decl_span_with_attachments`. Everything downstream depends on it:
//! a `get` that drops `@react.component` returns code that does not compile, and an `rm` that
//! misses a decorator orphans it onto the next declaration and silently breaks the file.

use resq::parser::{
    DECLARATION_KINDS, attachments, decl_full_span, decl_span_with_attachments,
    declaration_from_node, ensure_clean_parse, first_error_location, is_doc_comment,
    module_alias_parts, module_body_block, parse,
};
use resq::writer::{atomic_write, validate_output, validated_write};
use resq::{
    BinderKind, Declaration, DeclarationKind, FileSummary, ModuleAlias, ModulePath,
    module_name_from_path,
};
use std::fs;
use std::path::Path;
use tree_sitter::{Node, Tree};

const MAIN: &str = "tests/fixtures/proj/src/Main.res";
const VIEW: &str = "tests/fixtures/proj/src/View.res";
const TYPES: &str = "tests/fixtures/proj/src/Types.res";
const BROKEN: &str = "tests/fixtures/broken.res";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// Find the first declaration node anywhere in the tree that binds `name`.
fn find_decl_node<'a>(tree: &'a Tree, src: &str, name: &str) -> Node<'a> {
    fn walk<'a>(node: Node<'a>, src: &str, name: &str) -> Option<Node<'a>> {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(decl) = declaration_from_node(child, src, &ModulePath::root())
                && decl.names.iter().any(|n| n == name)
            {
                return Some(child);
            }
            if let Some(found) = walk(child, src, name) {
                return Some(found);
            }
        }
        None
    }
    walk(tree.root_node(), src, name)
        .unwrap_or_else(|| panic!("no declaration binding `{name}` found"))
}

/// The source text of a declaration including everything attached to it.
fn attached_source(tree: &Tree, src: &str, name: &str) -> String {
    let node = find_decl_node(tree, src, name);
    let (start, _) = decl_span_with_attachments(node, src);
    src[start..node.end_byte()].to_string()
}

// -------------------------------------------------------------------------------------------
// 1. Required: Main.res `entry` captures BOTH the doc comment and `@genType`.
// -------------------------------------------------------------------------------------------

#[test]
fn entry_span_captures_doc_comment_and_gentype() {
    let src = read(MAIN);
    let tree = parse(&src).unwrap();
    let node = find_decl_node(&tree, &src, "entry");

    // The bare node starts at `let`, one line below the decorator.
    assert_eq!(node.start_position().row + 1, 6, "bare declaration line");

    let (start_byte, start_line) = decl_span_with_attachments(node, &src);
    assert_eq!(
        start_line, 4,
        "span must start at the doc comment, not `let`"
    );

    let text = &src[start_byte..node.end_byte()];
    assert!(
        text.starts_with("/** Top-level entry point. */"),
        "doc comment not captured: {text:?}"
    );
    assert!(
        text.contains("@genType"),
        "decorator not captured: {text:?}"
    );
    assert!(text.ends_with("let entry = () => 1"), "text: {text:?}");

    let (decorators, doc) = attachments(node, &src);
    assert_eq!(decorators, vec!["@genType".to_string()]);
    assert_eq!(doc.as_deref(), Some("/** Top-level entry point. */"));

    let decl = declaration_from_node(node, &src, &ModulePath::root()).unwrap();
    assert_eq!(decl.start_line, 4, "start_line includes the attachments");
    assert_eq!(decl.end_line, 6);
    assert_eq!(decl.decorators, vec!["@genType".to_string()]);
    assert!(decl.doc_comment.is_some());
}

// -------------------------------------------------------------------------------------------
// 2. Required: View.res `make` captures `@react.component`.
// -------------------------------------------------------------------------------------------

#[test]
fn view_make_span_captures_react_component() {
    let src = read(VIEW);
    let tree = parse(&src).unwrap();
    let node = find_decl_node(&tree, &src, "make");

    let (start_byte, start_line) = decl_span_with_attachments(node, &src);
    assert_eq!(start_byte, 0);
    assert_eq!(start_line, 1);

    let text = &src[start_byte..node.end_byte()];
    assert!(
        text.starts_with("@react.component\nlet make"),
        "decorator not captured: {text:?}"
    );

    let decl = declaration_from_node(node, &src, &ModulePath::root()).unwrap();
    assert_eq!(decl.decorators, vec!["@react.component".to_string()]);
    assert_eq!(decl.names, vec!["make".to_string()]);
    assert_eq!(decl.kind, DeclarationKind::Let);
    assert_eq!(decl.binder_kind, BinderKind::Simple);
    assert!(decl.doc_comment.is_none());
}

// -------------------------------------------------------------------------------------------
// 3. Required negative cases: attachments separated by a blank line.
// -------------------------------------------------------------------------------------------

#[test]
fn comment_separated_by_blank_line_is_not_captured() {
    let src = "/** Free-standing prose, not documentation. */\n\nlet x = 1\n";
    let tree = parse(src).unwrap();
    let node = find_decl_node(&tree, src, "x");

    let (start_byte, start_line) = decl_span_with_attachments(node, src);
    assert_eq!(start_line, 3, "span must start at `let`, not the comment");
    assert_eq!(&src[start_byte..node.end_byte()], "let x = 1");

    let (decorators, doc) = attachments(node, src);
    assert!(decorators.is_empty());
    assert_eq!(
        doc, None,
        "a comment across a blank line is not a doc comment for this decl"
    );
}

#[test]
fn non_doc_block_comment_is_not_captured() {
    let src = "/* just a note */\nlet x = 1\n";
    let tree = parse(src).unwrap();
    let node = find_decl_node(&tree, src, "x");
    let (_, start_line) = decl_span_with_attachments(node, src);
    assert_eq!(start_line, 2);
    assert_eq!(attachments(node, src).1, None);
}

#[test]
fn line_comment_is_not_captured() {
    let src = "// a line comment\nlet x = 1\n";
    let tree = parse(src).unwrap();
    let node = find_decl_node(&tree, src, "x");
    assert_eq!(decl_span_with_attachments(node, src).1, 2);
}

#[test]
fn preceding_declaration_is_not_captured() {
    let src = "let a = 1\nlet b = 2\n";
    let tree = parse(src).unwrap();
    let node = find_decl_node(&tree, src, "b");
    let (start_byte, start_line) = decl_span_with_attachments(node, src);
    assert_eq!(start_line, 2);
    assert_eq!(&src[start_byte..node.end_byte()], "let b = 2");
}

#[test]
fn doc_comment_across_blank_line_stops_the_walk_but_decorator_still_travels() {
    // `@genType` binds to `x` even across a blank line, so dropping it on `rm` would corrupt the
    // file; the free-standing comment above it is not documentation and must stay put.
    let src = "/** prose */\n\n@genType\nlet x = 1\n";
    let tree = parse(src).unwrap();
    let node = find_decl_node(&tree, src, "x");

    let (start_byte, start_line) = decl_span_with_attachments(node, src);
    assert_eq!(start_line, 3, "start at the decorator, not the comment");
    assert_eq!(&src[start_byte..node.end_byte()], "@genType\nlet x = 1");

    let (decorators, doc) = attachments(node, src);
    assert_eq!(decorators, vec!["@genType".to_string()]);
    assert_eq!(doc, None);
}

#[test]
fn decorator_across_blank_line_is_still_captured() {
    // Deliberate refinement of "stop at the first blank-line gap": a decorator is grammatically
    // bound to the next declaration whatever the whitespace, and orphaning it breaks the file.
    let src = "@genType\n\nlet x = 1\n";
    let tree = parse(src).unwrap();
    let node = find_decl_node(&tree, src, "x");
    let (start_byte, start_line) = decl_span_with_attachments(node, src);
    assert_eq!(start_line, 1);
    assert_eq!(&src[start_byte..node.end_byte()], "@genType\n\nlet x = 1");
}

#[test]
fn multiple_decorators_are_captured_in_source_order() {
    let src = "/** doc */\n@genType\n@react.component\nlet make = () => 1\n";
    let tree = parse(src).unwrap();
    let node = find_decl_node(&tree, src, "make");
    let (decorators, doc) = attachments(node, src);
    assert_eq!(decorators, vec!["@genType", "@react.component"]);
    assert_eq!(doc.as_deref(), Some("/** doc */"));
    assert_eq!(decl_span_with_attachments(node, src).1, 1);
}

#[test]
fn nested_declaration_attachments_stop_at_the_module_brace() {
    // Inside `module Inner = { … }`, the first declaration's previous sibling is `{`.
    let src = read(MAIN);
    let tree = parse(&src).unwrap();
    let node = find_decl_node(&tree, &src, "helper");
    let (_, start_line) = decl_span_with_attachments(node, &src);
    assert_eq!(start_line, 21, "the nested doc comment is captured");
    assert_eq!(
        attachments(node, &src).1.as_deref(),
        Some("/** Nested helper. */")
    );

    let deep = find_decl_node(&tree, &src, "deepValue");
    assert_eq!(
        decl_span_with_attachments(deep, &src).1,
        25,
        "no attachment: must not reach past the opening brace"
    );
}

// -------------------------------------------------------------------------------------------
// 4. Required: `validate_output` rejects tests/fixtures/broken.res.
// -------------------------------------------------------------------------------------------

#[test]
fn validate_output_rejects_broken_fixture() {
    let src = read(BROKEN);
    let file = Path::new(BROKEN);
    let err = validate_output(&src, file, "set decl").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("broken.res"), "must name the file: {msg}");
    assert!(msg.contains("set decl"), "must name the operation: {msg}");
    assert!(msg.contains("does not parse"), "message: {msg}");
    assert!(
        msg.contains(':') && msg.contains("unchanged"),
        "must report line:col and that nothing was written: {msg}"
    );
}

#[test]
fn validate_output_accepts_clean_source() {
    let src = read(MAIN);
    assert!(validate_output(&src, Path::new(MAIN), "patch").is_ok());
}

#[test]
fn ensure_clean_parse_rejects_broken_fixture() {
    let src = read(BROKEN);
    let err = ensure_clean_parse(&src, Path::new(BROKEN)).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("refusing to edit"), "message: {msg}");
    assert!(msg.contains("broken.res"), "message: {msg}");
}

#[test]
fn ensure_clean_parse_accepts_every_good_fixture() {
    for path in [MAIN, VIEW, TYPES, "tests/fixtures/proj/src/Modern.res"] {
        let src = read(path);
        assert!(
            ensure_clean_parse(&src, Path::new(path)).is_ok(),
            "{path} should parse cleanly"
        );
    }
}

/// The known upstream grammar gaps (SPEC §0.1) must be *refused*, not worked around.
#[test]
fn known_grammar_gaps_are_refused_not_corrupted() {
    let src = "let lnot = x => lxor(x, -1n)\n";
    assert!(ensure_clean_parse(src, Path::new("Gap.res")).is_err());
    assert!(validate_output(src, Path::new("Gap.res"), "rm decl").is_err());
}

// -------------------------------------------------------------------------------------------
// 5. Required: the destructuring binding in Main.res.
// -------------------------------------------------------------------------------------------

#[test]
fn destructuring_let_binds_two_names() {
    let src = read(MAIN);
    let tree = parse(&src).unwrap();
    let node = find_decl_node(&tree, &src, "first");
    let decl = declaration_from_node(node, &src, &ModulePath::root()).unwrap();

    assert_eq!(decl.names.len(), 2, "names: {:?}", decl.names);
    assert_eq!(decl.names, vec!["first".to_string(), "second".to_string()]);
    assert_eq!(decl.binder_kind, BinderKind::Destructuring);
    assert_eq!(decl.kind, DeclarationKind::Let);
    assert_eq!(decl.start_line, 8);
    assert_eq!(decl.end_line, 8);

    // Addressable under either name.
    assert!(decl.is_at(&ModulePath::parse("first")));
    assert!(decl.is_at(&ModulePath::parse("second")));
    assert!(!decl.is_at(&ModulePath::parse("third")));
}

#[test]
fn destructuring_shapes() {
    let cases: &[(&str, &[&str])] = &[
        ("let {x, y} = point", &["x", "y"]),
        // `{x: a}` binds `a`, not the field name `x`.
        ("let {x: a, y: b} = point", &["a", "b"]),
        ("let {p: {q}, r} = v", &["q", "r"]),
        ("let [a, b] = arr", &["a", "b"]),
        ("let list{a, ...rest} = xs", &["a", "rest"]),
        ("let (a, b) as whole = pair", &["a", "b", "whole"]),
        ("let {x, _} = v", &["x"]),
    ];
    for (src, expected) in cases {
        let tree = parse(src).unwrap();
        let node = tree.root_node().named_child(0).unwrap();
        let decl = declaration_from_node(node, src, &ModulePath::root()).unwrap();
        assert_eq!(decl.names, *expected, "for {src}");
        assert_eq!(decl.binder_kind, BinderKind::Destructuring, "for {src}");
    }
}

/// `let a = 1 and b = 2` binds two names but through two *simple* binders.
#[test]
fn multiple_simple_bindings_stay_simple() {
    let src = "let a = 1 and b = 2";
    let tree = parse(src).unwrap();
    let node = tree.root_node().named_child(0).unwrap();
    let decl = declaration_from_node(node, src, &ModulePath::root()).unwrap();
    assert_eq!(decl.names, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(decl.binder_kind, BinderKind::Simple);
}

// -------------------------------------------------------------------------------------------
// first_error_location
// -------------------------------------------------------------------------------------------

#[test]
fn first_error_location_is_none_for_clean_source() {
    let src = read(MAIN);
    let tree = parse(&src).unwrap();
    assert_eq!(first_error_location(&tree, &src), None);
}

#[test]
fn first_error_location_reports_broken_fixture() {
    let src = read(BROKEN);
    let tree = parse(&src).unwrap();
    let (line, col) = first_error_location(&tree, &src).expect("broken.res must report a location");
    assert!(line >= 1 && col >= 1);
    assert!(line <= 3, "line {line} outside the fixture");
}

/// Regression guard for the trap this helper exists to avoid: the outermost node spans the whole
/// file, so reporting it points at line 1 for a fault far below.
#[test]
fn first_error_location_picks_the_innermost_node() {
    let src = "let a = 1\nmodule M = {\n  let x = (\n}\n";
    let tree = parse(src).unwrap();
    let (line, _) = first_error_location(&tree, src).expect("expected an error location");
    assert_eq!(line, 3, "must point at the MISSING `)`, not the file start");
}

#[test]
fn first_error_location_finds_missing_nodes() {
    // No ERROR node here at all — only a MISSING token.
    let src = "let f = (\n";
    let tree = parse(src).unwrap();
    assert!(tree.root_node().has_error());
    assert!(first_error_location(&tree, src).is_some());
}

#[test]
fn line_col_counts_characters_not_bytes() {
    // The unicode line in Main.res must not produce a column past the end of the line.
    let src = "let s = \"héllo ✓\"\nlet t = (\n";
    let tree = parse(src).unwrap();
    let (line, col) = first_error_location(&tree, src).unwrap();
    assert_eq!(line, 2);
    assert!(col <= 11, "col {col} looks byte-based");
}

// -------------------------------------------------------------------------------------------
// is_doc_comment
// -------------------------------------------------------------------------------------------

#[test]
fn is_doc_comment_distinguishes_by_prefix_only() {
    let src = "/** doc */\n/* block */\n// line\nlet x = 1\n";
    let tree = parse(src).unwrap();
    let root = tree.root_node();
    let doc = root.named_child(0).unwrap();
    let block = root.named_child(1).unwrap();
    let line = root.named_child(2).unwrap();

    assert_eq!(doc.kind(), "block_comment");
    assert_eq!(
        block.kind(),
        "block_comment",
        "SPEC §1: no doc-comment node kind"
    );
    assert!(is_doc_comment(doc, src));
    assert!(!is_doc_comment(block, src));
    assert!(!is_doc_comment(line, src));
}

// -------------------------------------------------------------------------------------------
// declaration_from_node across every DeclarationKind
// -------------------------------------------------------------------------------------------

#[test]
fn declaration_kinds_are_recognised() {
    let cases: &[(&str, DeclarationKind, &str)] = &[
        ("let x = 1", DeclarationKind::Let, "x"),
        ("type msg = | A | B", DeclarationKind::Type, "msg"),
        ("type user = {name: string}", DeclarationKind::Type, "user"),
        ("type t", DeclarationKind::Type, "t"),
        ("module M = {let y = 1}", DeclarationKind::Module, "M"),
        ("module A = B.C", DeclarationKind::Module, "A"),
        (
            "external ev: string => unit = \"eval\"",
            DeclarationKind::External,
            "ev",
        ),
        ("include Belt.Array", DeclarationKind::Include, "Belt.Array"),
        ("open Belt", DeclarationKind::Open, "Belt"),
        ("open Belt.Array", DeclarationKind::Open, "Belt.Array"),
    ];
    for (src, kind, name) in cases {
        let tree = parse(src).unwrap();
        let node = tree.root_node().named_child(0).unwrap();
        let decl = declaration_from_node(node, src, &ModulePath::root())
            .unwrap_or_else(|| panic!("no declaration for {src}"));
        assert_eq!(decl.kind, *kind, "for {src}");
        assert_eq!(decl.names, vec![name.to_string()], "for {src}");
    }
}

/// A record type sits in an *unnamed* child of `type_binding` while a variant uses the `body:`
/// field (SPEC §1 finding 4) — matching on `body:` alone silently misses every record.
#[test]
fn record_and_variant_types_both_produce_declarations() {
    let src = read(TYPES);
    let tree = parse(&src).unwrap();
    for name in ["msg", "user", "id", "abstractThing"] {
        let node = find_decl_node(&tree, &src, name);
        let decl = declaration_from_node(node, &src, &ModulePath::root()).unwrap();
        assert_eq!(decl.kind, DeclarationKind::Type);
        assert_eq!(decl.names, vec![name.to_string()]);
    }
}

#[test]
fn type_and_declares_several_names() {
    let src = "type a = int and b = string";
    let tree = parse(src).unwrap();
    let node = tree.root_node().named_child(0).unwrap();
    let decl = declaration_from_node(node, src, &ModulePath::root()).unwrap();
    assert_eq!(decl.names, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn type_annotations_are_read_without_the_colon() {
    let src = "let n: int = 1\nexternal ev: string => unit = \"eval\"\nlet u = 1\n";
    let tree = parse(src).unwrap();
    let root = tree.root_node();
    let annotated =
        declaration_from_node(root.named_child(0).unwrap(), src, &ModulePath::root()).unwrap();
    assert_eq!(annotated.type_annotation.as_deref(), Some("int"));

    let ext =
        declaration_from_node(root.named_child(1).unwrap(), src, &ModulePath::root()).unwrap();
    assert_eq!(ext.type_annotation.as_deref(), Some("string => unit"));

    let plain =
        declaration_from_node(root.named_child(2).unwrap(), src, &ModulePath::root()).unwrap();
    assert_eq!(plain.type_annotation, None);
}

/// `.resi` signatures have no `body:`; they must still read as ordinary declarations (SPEC §1
/// finding 5) so `resi.rs` needs no separate grammar path.
#[test]
fn resi_signatures_read_as_declarations() {
    let path = "tests/fixtures/proj/src/View.resi";
    let src = read(path);
    let tree = parse(&src).unwrap();
    let node = find_decl_node(&tree, &src, "make");
    let decl = declaration_from_node(node, &src, &ModulePath::root()).unwrap();
    assert_eq!(decl.kind, DeclarationKind::Let);
    assert_eq!(decl.decorators, vec!["@react.component".to_string()]);
    assert!(
        decl.type_annotation
            .as_deref()
            .is_some_and(|t| t.contains("React.element")),
        "annotation: {:?}",
        decl.type_annotation
    );
}

#[test]
fn non_declaration_nodes_return_none() {
    let src = "/** doc */\n@genType\nlet x = 1\n";
    let tree = parse(src).unwrap();
    let root = tree.root_node();
    assert!(
        declaration_from_node(root.named_child(0).unwrap(), src, &ModulePath::root()).is_none()
    );
    assert!(
        declaration_from_node(root.named_child(1).unwrap(), src, &ModulePath::root()).is_none()
    );
}

// -------------------------------------------------------------------------------------------
// module_body_block / module_alias_parts
// -------------------------------------------------------------------------------------------

#[test]
fn module_body_and_alias_are_distinguished() {
    let src = read(MAIN);
    let tree = parse(&src).unwrap();

    let inner = find_decl_node(&tree, &src, "Inner");
    assert!(
        module_body_block(inner).is_some(),
        "`module Inner = {{…}}` has a body"
    );
    assert_eq!(module_alias_parts(inner, &src), None);

    let alias = find_decl_node(&tree, &src, "Arr");
    assert!(
        module_body_block(alias).is_none(),
        "an alias has no members"
    );
    assert_eq!(
        module_alias_parts(alias, &src),
        Some(("Arr".to_string(), "Belt.Array".to_string()))
    );
}

#[test]
fn functor_body_is_reachable() {
    let src = "module F = (X: S) => { let y = 1 }";
    let tree = parse(src).unwrap();
    let node = tree.root_node().named_child(0).unwrap();
    let block = module_body_block(node).expect("functor body");
    assert_eq!(block.kind(), "block");
}

// -------------------------------------------------------------------------------------------
// decl_full_span / DECLARATION_KINDS
// -------------------------------------------------------------------------------------------

/// The byte span an `rm` would cut must be exactly the text a `get` would print.
#[test]
fn decl_full_span_matches_the_attachment_span() {
    let src = read(MAIN);
    let tree = parse(&src).unwrap();
    let node = find_decl_node(&tree, &src, "entry");
    let (start, end) = decl_full_span(node, &src);
    assert_eq!(&src[start..end], attached_source(&tree, &src, "entry"));
    assert_eq!(start, decl_span_with_attachments(node, &src).0);
    assert_eq!(end, node.end_byte());
}

/// `DECLARATION_KINDS` is the published list downstream modules filter on; it must not drift out
/// of step with `declaration_kind`.
#[test]
fn declaration_kinds_const_matches_the_classifier() {
    let src = "let a = 1\ntype t = int\nmodule M = {}\nexternal e: int = \"e\"\n\
               include Belt.Array\nopen Belt\n";
    let tree = parse(src).unwrap();
    let root = tree.root_node();
    let mut cursor = root.walk();
    let kinds: Vec<&str> = root
        .named_children(&mut cursor)
        .filter(|c| declaration_from_node(*c, src, &ModulePath::root()).is_some())
        .map(|c| c.kind())
        .collect();
    for kind in &kinds {
        assert!(
            DECLARATION_KINDS.contains(kind),
            "`{kind}` is classified as a declaration but is missing from DECLARATION_KINDS"
        );
    }
    assert_eq!(kinds.len(), DECLARATION_KINDS.len());
}

// -------------------------------------------------------------------------------------------
// The pieces compose: a walk built only from spine helpers reproduces the fixture's structure.
// -------------------------------------------------------------------------------------------

fn summarize(path: &str, src: &str, tree: &Tree) -> FileSummary {
    fn walk(node: Node, src: &str, path: &ModulePath, out: &mut FileSummary) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let Some(decl) = declaration_from_node(child, src, path) else {
                continue;
            };
            match decl.kind {
                DeclarationKind::Open => out.opens.extend(decl.names.iter().cloned()),
                DeclarationKind::Module => {
                    if let Some((name, target)) = module_alias_parts(child, src) {
                        out.aliases.push(ModuleAlias { name, target });
                    }
                }
                _ => {}
            }
            let child_path = decl.names.first().map(|n| path.child(n));
            out.declarations.push(decl);
            if let (Some(block), Some(child_path)) = (module_body_block(child), child_path) {
                walk(block, src, &child_path, out);
            }
        }
    }

    let mut summary = FileSummary::new(module_name_from_path(Path::new(path)));
    walk(tree.root_node(), src, &ModulePath::root(), &mut summary);
    summary
}

#[test]
fn spine_helpers_compose_into_a_file_summary() {
    let src = read(MAIN);
    let tree = parse(&src).unwrap();
    let summary = summarize(MAIN, &src, &tree);

    assert_eq!(summary.module_name, "Main");
    assert_eq!(summary.opens, vec!["Belt".to_string()]);
    assert_eq!(
        summary.aliases,
        vec![ModuleAlias {
            name: "Arr".into(),
            target: "Belt.Array".into()
        }]
    );

    // Nested addressing (SPEC §3.1): a bare name never reaches a nested declaration.
    let deep = summary
        .find_declaration(&ModulePath::parse("Inner.Deep.deepValue"))
        .expect("Inner.Deep.deepValue");
    assert_eq!(deep.names, vec!["deepValue".to_string()]);
    assert_eq!(deep.path, ModulePath::parse("Inner.Deep"));

    assert!(
        summary
            .find_declaration(&ModulePath::parse("deepValue"))
            .is_none(),
        "a bare name must not match a nested declaration"
    );
    assert!(
        summary
            .find_declaration(&ModulePath::parse("helper"))
            .is_none()
    );
    assert!(
        summary
            .find_declaration(&ModulePath::parse("Inner.helper"))
            .is_some()
    );

    // Destructuring is addressable under either bound name.
    assert!(
        summary
            .find_declaration(&ModulePath::parse("first"))
            .is_some()
    );
    assert!(
        summary
            .find_declaration(&ModulePath::parse("second"))
            .is_some()
    );

    let entry = summary
        .find_declaration(&ModulePath::parse("entry"))
        .unwrap();
    assert_eq!(entry.primary_path().to_string(), "entry");
    assert_eq!(
        summary
            .find_declaration(&ModulePath::parse("Inner.Deep.deepValue"))
            .unwrap()
            .primary_path()
            .to_string(),
        "Inner.Deep.deepValue"
    );

    // The slice a `get` would emit must include the attachments and stand on its own.
    let text = attached_source(&tree, &src, "entry");
    assert!(text.contains("@genType") && text.contains("/** Top-level entry point. */"));
    assert!(!parse(text.as_str()).unwrap().root_node().has_error());
}

// -------------------------------------------------------------------------------------------
// ModulePath
// -------------------------------------------------------------------------------------------

#[test]
fn module_path_round_trips() {
    let p = ModulePath::parse("Inner.Deep.helper");
    assert_eq!(p.to_string(), "Inner.Deep.helper");
    assert_eq!(p.len(), 3);
    assert_eq!(p.leaf(), Some("helper"));

    let (parent, leaf) = p.split_leaf().unwrap();
    assert_eq!(parent.to_string(), "Inner.Deep");
    assert_eq!(leaf, "helper");

    assert_eq!(parent.child("helper"), p);
    assert!(ModulePath::root().is_empty());
    assert_eq!(ModulePath::root().to_string(), "");
    assert_eq!(ModulePath::root().split_leaf(), None);
    assert_eq!(
        "a.b".parse::<ModulePath>().unwrap(),
        ModulePath::parse("a.b")
    );
    // Empty segments cannot match anything, so they are dropped rather than kept.
    assert_eq!(ModulePath::parse("A..b").to_string(), "A.b");
    assert_eq!(ModulePath::parse("").len(), 0);
}

#[test]
fn module_name_is_derived_from_the_file_name() {
    assert_eq!(module_name_from_path(Path::new("src/Main.res")), "Main");
    assert_eq!(module_name_from_path(Path::new("src/View.resi")), "View");
    assert_eq!(
        module_name_from_path(Path::new("src/nested/util.res")),
        "Util"
    );
}

// -------------------------------------------------------------------------------------------
// Serialization shape
// -------------------------------------------------------------------------------------------

#[test]
fn optional_fields_are_omitted_from_json() {
    let bare = Declaration {
        names: vec!["x".into()],
        path: ModulePath::parse("Inner"),
        kind: DeclarationKind::Let,
        binder_kind: BinderKind::Simple,
        decorators: Vec::new(),
        type_annotation: None,
        doc_comment: None,
        start_line: 1,
        end_line: 1,
    };
    let json = serde_json::to_string(&bare).unwrap();
    assert!(!json.contains("type_annotation"), "{json}");
    assert!(!json.contains("doc_comment"), "{json}");
    assert!(!json.contains("decorators"), "{json}");
    // ModulePath serializes as the dotted string the CLI accepts, not an array.
    assert!(json.contains("\"path\":\"Inner\""), "{json}");
    assert!(json.contains("\"kind\":\"let\""), "{json}");
    assert!(json.contains("\"binder_kind\":\"simple\""), "{json}");

    let full = Declaration {
        decorators: vec!["@genType".into()],
        type_annotation: Some("int".into()),
        doc_comment: Some("/** d */".into()),
        ..bare
    };
    let json = serde_json::to_string(&full).unwrap();
    assert!(json.contains("\"decorators\":[\"@genType\"]"), "{json}");
    assert!(json.contains("\"type_annotation\":\"int\""), "{json}");
}

#[test]
fn kind_display_matches_the_cli_vocabulary() {
    assert_eq!(DeclarationKind::External.to_string(), "external");
    assert_eq!(DeclarationKind::Module.to_string(), "module");
    assert_eq!(BinderKind::Destructuring.to_string(), "destructuring");
}

// -------------------------------------------------------------------------------------------
// atomic_write / validated_write
// -------------------------------------------------------------------------------------------

#[test]
fn atomic_write_replaces_the_file_and_leaves_no_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Out.res");
    fs::write(&file, "let old = 1\n").unwrap();

    atomic_write(&file, "let new = 2\n").unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "let new = 2\n");

    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "Out.res")
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

#[test]
fn atomic_write_creates_a_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("New.res");
    atomic_write(&file, "let x = 1\n").unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "let x = 1\n");
}

#[test]
fn validated_write_leaves_the_file_untouched_when_the_output_is_broken() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Guard.res");
    let original = "let good = 1\n";
    fs::write(&file, original).unwrap();

    let err =
        validated_write(&file, "let broken = (x => {\n  let y = x +\n", "set decl").unwrap_err();
    assert!(err.to_string().contains("set decl"), "{err}");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        original,
        "the on-disk file must be byte-for-byte unchanged"
    );
}

#[test]
fn validated_write_commits_a_good_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Ok.res");
    fs::write(&file, "let a = 1\n").unwrap();
    validated_write(&file, "@genType\nlet a = 2\n", "set decl").unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "@genType\nlet a = 2\n");
}

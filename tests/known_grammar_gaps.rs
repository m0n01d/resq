//! Conductor-owned. INVERTED tests: these assert constructs that the pinned grammar
//! currently FAILS to parse.
//!
//! Found by running the parser over the ReScript compiler repo (3,181 real `.res`/`.resi`
//! files) on 2026-08-01 with tree-sitter 0.26 + tree-sitter-rescript v6.0.0.
//!
//! These are UPSTREAM GRAMMAR BUGS, not resq bugs. Do not attempt to work around them in
//! resq — resq's write-safety invariant (SPEC §2) already handles them correctly by
//! refusing to edit a file it cannot parse, which is the safe failure mode.
//!
//! If one of these starts PASSING, this test fails loudly — that means the grammar was
//! fixed upstream and the pin should be bumped and this entry removed.

const KNOWN_GAPS: &[(&str, &str)] = &[
    // 1. ReScript 12.3 deprecation-migration extension with a bare type payload.
    //    Appears in 26 of the 28 failing files under packages/@rescript/runtime —
    //    essentially all of the deprecated Js_* compatibility shims.
    (
        "%replace.type with bare type payload",
        "@deprecated({\n  reason: \"x\",\n  migrate: %replace.type(: Map.t),\n})\ntype t<'k> = M.t<'k>",
    ),
    // 2. Negative bigint literal. `42n` parses; `-1n` does not. `-1` parses.
    ("negative bigint literal", "let lnot = x => lxor(x, -1n)"),
    // 3. Two or more consecutive trailing block comments closing a module block.
    //    One trailing comment is fine; two is not.
    (
        "two trailing comments at end of module block",
        "module M = {\n  let x = 1\n  /* one */\n  /* two */\n}",
    ),
];

#[test]
fn known_gaps_still_fail() {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_rescript::LANGUAGE.into()).unwrap();
    let mut now_passing = Vec::new();
    for (label, src) in KNOWN_GAPS {
        let tree = p.parse(*src, None).unwrap();
        if !tree.root_node().has_error() {
            now_passing.push(*label);
        }
    }
    assert!(
        now_passing.is_empty(),
        "grammar gap(s) now PARSE CLEANLY — upstream fixed them. Bump the \
         tree-sitter-rescript pin and delete these entries from KNOWN_GAPS: {now_passing:?}"
    );
}

/// Guards the claim that these are narrow gaps rather than broad breakage: the
/// neighbouring constructs must still parse.
#[test]
fn gaps_are_narrow() {
    let must_pass: &[(&str, &str)] = &[
        ("positive bigint", "let b = 42n"),
        ("negative int", "let f = x => lxor(x, -1)"),
        ("plain @deprecated", r#"@deprecated("use X") let f = 1"#),
        (
            "@deprecated record without %replace.type",
            "@deprecated({reason: \"x\"})\ntype t = int",
        ),
        (
            "single trailing comment in module",
            "module M = {\n  let x = 1\n  /* one */\n}",
        ),
    ];
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_rescript::LANGUAGE.into()).unwrap();
    for (label, src) in must_pass {
        let tree = p.parse(*src, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "regression: `{label}` should parse but does not"
        );
    }
}

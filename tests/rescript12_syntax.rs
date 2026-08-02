//! Conductor-owned. Asserts the pinned grammar parses ReScript 12 syntax.
//!
//! Verified against ReScript 12.3.0 (latest stable as of 2026-08-01) with
//! tree-sitter 0.26 + tree-sitter-rescript v6.0.0. If any case here regresses,
//! the grammar pin is wrong for the language version we target.

const CASES: &[(&str, &str)] = &[
    ("dict literal (v12)", r#"let d = dict{"a": 1, "b": 2}"#),
    ("async/await", "let f = async () => {\n  let x = await g()\n  x\n}"),
    ("regex literal", "let re = /ab+c/gi"),
    ("tagged template", "let q = sql`SELECT * FROM t`"),
    ("optional record field", "type t = {name: string, age?: int}"),
    ("record spread", "let b = {...a, x: 1}"),
    ("variant spread", "type c = | ...a | Extra"),
    ("@tag untagged variant", "@tag(\"kind\") type t = | @as(\"a\") A | B({x: int})"),
    ("@unboxed", "@unboxed type t = Str(string) | Num(float)"),
    ("let rec and", "let rec even = x => x == 0 || odd(x - 1)\nand odd = x => x != 0 && even(x - 1)"),
    ("try/catch", "let f = () => try danger() catch { | Not_found => 0 }"),
    ("pipe chains", "let r = xs->Array.map(f)->Array.filter(g)->Array.length"),
    ("labeled punning", "let v = make(~name, ~count, ())"),
    ("object type", "type o = {\"a\": int, \"b\": string}"),
    ("polyvar + coercion", "let c: [> #red] = #red\nlet n = (x :> int)"),
    ("%%raw block", "%%raw(`function ext() { return 1 }`)"),
    ("module type + functor", "module type S = { let x: int }\nmodule F = (M: S) => { let y = M.x }"),
    ("first-class module", "let m = module(Foo: S)"),
    ("bigint (v12)", "let b = 42n"),
    ("dict pattern", "let f = d => switch d { | dict{\"a\": v} => v | _ => 0 }"),
    ("Stdlib qualified", "let s = Stdlib.Array.make(~length=3, 0)"),
    ("uncurried explicit", "let f = (. x) => x"),
];

#[test]
fn parses_all_rescript_12_syntax() {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_rescript::LANGUAGE.into()).unwrap();
    let mut failures = Vec::new();
    for (label, src) in CASES {
        let tree = p.parse(*src, None).unwrap();
        if tree.root_node().has_error() {
            failures.push(format!("{label}\n    {}", tree.root_node().to_sexp()));
        }
    }
    assert!(
        failures.is_empty(),
        "grammar failed on {} ReScript 12 construct(s):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

//! Tests for `src/project.rs` (agent A4): project-root discovery, config parsing (all three
//! `sources` shapes), source-file walking, `file_to_module_name`, and `.res`/`.resi` pairing.

use resq::project::{Namespace, Project, ProjectConfig, file_to_module_name, find_sibling};
use std::path::{Path, PathBuf};

const PROJ_ROOT: &str = "tests/fixtures/proj";
const NESTED_UTIL: &str = "tests/fixtures/proj/src/nested/Util.res";

/// Root discovery from a nested path finds the fixture project root.
#[test]
fn discover_from_nested_path_finds_project_root() {
    let project = Project::discover(Path::new(NESTED_UTIL)).expect("should find a project root");
    let expected_root = std::fs::canonicalize(PROJ_ROOT).expect("fixture root must exist");
    let actual_root = std::fs::canonicalize(&project.root).expect("discovered root must exist");
    assert_eq!(actual_root, expected_root);
    assert_eq!(project.config_path.file_name().unwrap(), "rescript.json");
}

/// Root discovery also works starting from a directory rather than a file.
#[test]
fn discover_from_directory_path_finds_project_root() {
    let project =
        Project::discover(Path::new("tests/fixtures/proj/src/nested")).expect("should find root");
    let actual_root = std::fs::canonicalize(&project.root).unwrap();
    let expected_root = std::fs::canonicalize(PROJ_ROOT).unwrap();
    assert_eq!(actual_root, expected_root);
}

/// Source walking finds all 6 fixture source files (5 `.res` + 1 `.resi`), including the one
/// nested under `src/nested/`.
#[test]
fn source_walking_finds_all_fixture_files() {
    let project = Project::discover(Path::new(NESTED_UTIL)).unwrap();
    let files = project.source_files().expect("walking sources should succeed");

    let names: Vec<String> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();

    for expected in [
        "Main.res",
        "Modern.res",
        "Types.res",
        "View.res",
        "View.resi",
        "Util.res",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "expected {expected} in walked files, got {names:?}"
        );
    }
    assert_eq!(files.len(), 6, "expected exactly 6 fixture source files, got {names:?}");

    // The nested file must actually be found *under* nested/, not just by basename.
    assert!(
        files.iter().any(|p| p.ends_with("nested/Util.res")),
        "Util.res should be discovered under src/nested/"
    );
}

/// The single most important rule in this module: module name is the file basename, capitalized,
/// regardless of directory depth. `src/nested/Util.res` is module `Util`, never `Nested.Util`.
#[test]
fn file_to_module_name_ignores_directory_nesting() {
    assert_eq!(
        file_to_module_name(Path::new("src/nested/Util.res")),
        "Util"
    );
    assert_eq!(file_to_module_name(Path::new("src/Main.res")), "Main");
    assert_eq!(file_to_module_name(Path::new("View.resi")), "View");
    assert_eq!(
        file_to_module_name(Path::new("a/b/c/d/lowercase.res")),
        "Lowercase"
    );
}

/// `sources` as a bare string.
#[test]
fn parses_sources_as_bare_string() {
    let config = ProjectConfig::parse(r#"{ "name": "proj", "sources": "src" }"#)
        .expect("bare string sources should parse");
    assert_eq!(config.sources.len(), 1);
    assert_eq!(config.sources[0].dir, "src");
    assert!(!config.sources[0].subdirs);
}

/// `sources` as a single object, with `subdirs`.
#[test]
fn parses_sources_as_object() {
    let config =
        ProjectConfig::parse(r#"{ "name": "proj", "sources": { "dir": "src", "subdirs": true } }"#)
            .expect("object sources should parse");
    assert_eq!(config.sources.len(), 1);
    assert_eq!(config.sources[0].dir, "src");
    assert!(config.sources[0].subdirs);
}

/// `sources` as an array mixing bare strings and objects.
#[test]
fn parses_sources_as_array_of_mixed_shapes() {
    let config = ProjectConfig::parse(
        r#"{
            "name": "proj",
            "sources": [
                "shared",
                { "dir": "src", "subdirs": true },
                { "dir": "vendor" }
            ]
        }"#,
    )
    .expect("array sources should parse");
    assert_eq!(config.sources.len(), 3);
    assert_eq!(config.sources[0].dir, "shared");
    assert!(!config.sources[0].subdirs);
    assert_eq!(config.sources[1].dir, "src");
    assert!(config.sources[1].subdirs);
    assert_eq!(config.sources[2].dir, "vendor");
    assert!(!config.sources[2].subdirs);
}

/// `namespace: true` derives a namespace from `name`; a string is used verbatim; absent means none.
#[test]
fn parses_namespace_variants() {
    let auto = ProjectConfig::parse(r#"{ "name": "my-app", "sources": "src", "namespace": true }"#)
        .unwrap();
    assert_eq!(auto.namespace, Namespace::Named("MyApp".to_string()));

    let named =
        ProjectConfig::parse(r#"{ "sources": "src", "namespace": "CustomNs" }"#).unwrap();
    assert_eq!(named.namespace, Namespace::Named("CustomNs".to_string()));

    let none = ProjectConfig::parse(r#"{ "sources": "src" }"#).unwrap();
    assert_eq!(none.namespace, Namespace::None);

    let explicit_false =
        ProjectConfig::parse(r#"{ "sources": "src", "namespace": false }"#).unwrap();
    assert_eq!(explicit_false.namespace, Namespace::None);
}

/// `suffix` is carried through verbatim.
#[test]
fn parses_suffix() {
    let config =
        ProjectConfig::parse(r#"{ "sources": "src", "suffix": ".res.mjs" }"#).unwrap();
    assert_eq!(config.suffix.as_deref(), Some(".res.mjs"));

    let default = ProjectConfig::parse(r#"{ "sources": "src" }"#).unwrap();
    assert_eq!(default.suffix, None);
}

/// The fixture's actual `rescript.json` parses to the expected normalized shape.
#[test]
fn parses_fixture_rescript_json() {
    let text = std::fs::read_to_string("tests/fixtures/proj/rescript.json").unwrap();
    let config = ProjectConfig::parse(&text).expect("fixture config should parse");
    assert_eq!(config.sources.len(), 1);
    assert_eq!(config.sources[0].dir, "src");
    assert!(config.sources[0].subdirs);
    assert_eq!(config.suffix.as_deref(), Some(".res.mjs"));
}

/// Missing `sources` field is a clear error, not a panic.
#[test]
fn missing_sources_field_is_a_clear_error() {
    let err = ProjectConfig::parse(r#"{ "name": "proj" }"#).expect_err("should error");
    assert!(
        err.to_string().contains("sources"),
        "error should mention the missing field, got: {err}"
    );
}

/// Malformed JSON is a clear error, not a panic.
#[test]
fn malformed_json_is_a_clear_error() {
    let err = ProjectConfig::parse("{ not json").expect_err("should error");
    assert!(!err.to_string().is_empty());
}

/// A path with no `rescript.json`/`bsconfig.json` in any ancestor returns a clear error rather
/// than panicking. Uses a fresh temp directory so the real filesystem's ancestry can't
/// accidentally supply a config.
#[test]
fn missing_config_returns_clear_error() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    // Canonicalize so `/tmp` symlink weirdness on macOS doesn't affect the walk, and use a nested
    // subdirectory so there's real ancestry to walk up through within the temp root.
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let nested = root.join("a/b/c");
    std::fs::create_dir_all(&nested).unwrap();
    let some_file = nested.join("NoProject.res");
    std::fs::write(&some_file, "let x = 1\n").unwrap();

    let err = Project::discover(&some_file).expect_err("should not find a project root");
    let msg = err.to_string();
    assert!(
        msg.contains("rescript.json") && msg.contains("bsconfig.json"),
        "error should name both config files it looked for, got: {msg}"
    );
}

/// `rescript.json` is preferred over `bsconfig.json` when both exist in the same directory.
#[test]
fn prefers_rescript_json_over_bsconfig_json() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::write(
        root.join("rescript.json"),
        r#"{ "sources": "modern-src" }"#,
    )
    .unwrap();
    std::fs::write(root.join("bsconfig.json"), r#"{ "sources": "old-src" }"#).unwrap();

    let project = Project::discover(&root).expect("should discover the project");
    assert_eq!(project.config_path.file_name().unwrap(), "rescript.json");
    assert_eq!(project.config.sources[0].dir, "modern-src");
}

/// `View.res` has a sibling `View.resi` in the fixture project; the reverse direction also works.
#[test]
fn find_sibling_pairs_res_and_resi() {
    let res: PathBuf = "tests/fixtures/proj/src/View.res".into();
    let resi: PathBuf = "tests/fixtures/proj/src/View.resi".into();

    let found_resi = find_sibling(&res).expect("View.res should have a sibling View.resi");
    assert!(found_resi.ends_with("View.resi"));

    let found_res = find_sibling(&resi).expect("View.resi should have a sibling View.res");
    assert!(found_res.ends_with("View.res"));
}

/// A `.res` file with no sibling `.resi` (e.g. `Main.res`) yields `None`, not an error.
#[test]
fn find_sibling_is_none_when_no_pair_exists() {
    let main: PathBuf = "tests/fixtures/proj/src/Main.res".into();
    assert_eq!(find_sibling(&main), None);
}

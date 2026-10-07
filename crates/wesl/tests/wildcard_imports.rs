//! Integration tests for wildcard imports (`import path::*;`).

use wesl::{
    CompileResult, Compiler, Error, ManglerKind,
    error::{UsageError, Warning},
    resolver::VirtualResolver,
    syntax::{ModulePath, PathOrigin},
};

/// Parse a module path, where the package name may contain a `/` for nested dependencies.
fn module_path(path: &str) -> ModulePath {
    match path.split_once("::") {
        Some((package, rest)) if package.contains('/') => ModulePath::new(
            PathOrigin::Package(package.to_string()),
            rest.split("::").map(str::to_string).collect(),
        ),
        _ => path.parse().unwrap(),
    }
}

fn compile(main: &str, modules: &[(&str, &str)]) -> Result<CompileResult, Error> {
    let mut resolver = VirtualResolver::new();
    resolver.add_module("package".parse().unwrap(), main.to_string().into());
    for (path, source) in modules {
        resolver.add_module(module_path(path), source.to_string().into());
    }
    let mut compiler = Compiler::default().with_resolver(resolver);
    compiler.options.mangler = ManglerKind::None;
    compiler.options.keep_main = true;
    compiler.compile_root()
}

fn squash(wgsl: &str) -> String {
    wgsl.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn output(main: &str, modules: &[(&str, &str)]) -> String {
    let result = compile(main, modules)
        .inspect_err(|e| eprintln!("{e}"))
        .unwrap();
    squash(&result.to_string())
}

fn error(main: &str, modules: &[(&str, &str)]) -> Error {
    match compile(main, modules) {
        Ok(result) => panic!("expected an error, got:\n{result}"),
        Err(err) => err,
    }
}

fn warnings(main: &str, modules: &[(&str, &str)]) -> Vec<Warning> {
    compile(main, modules)
        .inspect_err(|e| eprintln!("{e}"))
        .unwrap()
        .warnings
}

fn usage_error(err: Error) -> UsageError {
    match err {
        Error::UsageError(err) => err,
        Error::Error(diagnostic) => match *diagnostic.error {
            Error::UsageError(err) => err,
            other => panic!("expected a usage error, got: {other}"),
        },
        other => panic!("expected a usage error, got: {other}"),
    }
}

#[test]
fn imports_visible_items() {
    let out = output(
        "import package::other::*; fn f() -> u32 { return helper() + x; }",
        &[(
            "package::other",
            "const x: u32 = 1; fn helper() -> u32 { return x; } fn unused() {}",
        )],
    );
    assert_eq!(
        out,
        "fn f() -> u32 { return helper() + x; } const x: u32 = 1; fn helper() -> u32 { return x; }"
    );
}

#[test]
fn imports_types() {
    let out = output(
        "import package::types::*; fn f(p: Point) -> array<Point, 2> { var a: array<Point, 2>; return a; }",
        &[("package::types", "struct Point { x: f32 }")],
    );
    assert!(out.contains("struct Point { x: f32 }"));
}

#[test]
fn imports_with_relative_paths() {
    let out = output(
        "import package::a::*; fn f() -> u32 { return value(); }",
        &[
            (
                "package::a",
                "import super::b::*; fn value() -> u32 { return inner(); }",
            ),
            ("package::b", "fn inner() -> u32 { return 1; }"),
        ],
    );
    assert!(out.contains("fn inner() -> u32 { return 1; }"));
}

#[test]
fn private_items_are_not_imported() {
    let err = error(
        "import package::other::*; fn f() -> u32 { return secret(); }",
        &[("package::other", "private fn secret() -> u32 { return 1; }")],
    );
    assert!(
        err.to_string()
            .contains("cannot find declaration of `secret`")
    );
}

#[test]
fn package_items_are_not_imported_across_packages() {
    let err = error(
        "import lib::prelude::*; fn f() -> u32 { return hidden(); }",
        &[(
            "lib::prelude",
            "@!wildcardable; public fn shown() {} fn hidden() -> u32 { return 1; }",
        )],
    );
    assert!(
        err.to_string()
            .contains("cannot find declaration of `hidden`")
    );
}

#[test]
fn submodule_names_are_not_imported() {
    let err = error(
        "import package::*; fn f() -> u32 { return other::value(); }",
        &[("package::other", "fn value() -> u32 { return 1; }")],
    );
    assert!(err.to_string().contains("other"), "{err}");
}

#[test]
fn submodule_items_are_not_imported() {
    let err = error(
        "import package::parent::*; fn f() -> u32 { return value(); }",
        &[
            ("package::parent", "fn own() {}"),
            ("package::parent::child", "fn value() -> u32 { return 1; }"),
        ],
    );
    assert!(
        err.to_string()
            .contains("cannot find declaration of `value`")
    );
}

#[test]
fn import_collection_with_wildcard_member() {
    let out = output(
        "import package::{a::first, b::*}; fn f() -> u32 { return first() + second(); }",
        &[
            (
                "package::a",
                "fn first() -> u32 { return 1; } fn other() {}",
            ),
            (
                "package::b",
                "fn second() -> u32 { return 2; } fn another() {}",
            ),
        ],
    );
    assert!(out.contains("fn first()"));
    assert!(out.contains("fn second()"));
    assert!(!out.contains("other"));
    assert!(!out.contains("another"));
}

#[test]
fn sibling_branch_does_not_widen_the_wildcard() {
    let err = error(
        "import package::foo::{a::deep, *}; fn f() -> u32 { return deep_helper(); }",
        &[
            ("package::foo", "fn own() {}"),
            (
                "package::foo::a",
                "fn deep() {} fn deep_helper() -> u32 { return 1; }",
            ),
        ],
    );
    assert!(
        err.to_string()
            .contains("cannot find declaration of `deep_helper`")
    );
}

#[test]
fn local_declaration_wins_over_wildcard() {
    let result = compile(
        "import package::other::*; fn value() -> u32 { return 2; } fn f() -> u32 { return value(); }",
        &[("package::other", "fn value() -> u32 { return 1; }")],
    )
    .unwrap();
    let out = squash(&result.to_string());
    assert!(out.contains("return 2;"));
    assert!(!out.contains("return 1;"));
    assert_eq!(
        result.warnings,
        vec![Warning::WildcardShadow {
            module: "package".parse().unwrap(),
            name: "value".to_string()
        }]
    );
}

#[test]
fn named_import_wins_over_wildcard() {
    let result = compile(
        "import package::a::value; import package::b::*; fn f() -> u32 { return value(); }",
        &[
            ("package::a", "fn value() -> u32 { return 1; }"),
            ("package::b", "fn value() -> u32 { return 2; }"),
        ],
    )
    .unwrap();
    let out = squash(&result.to_string());
    assert!(out.contains("return 1;"));
    assert!(!out.contains("return 2;"));
    assert_eq!(result.warnings.len(), 1);
}

#[test]
fn named_import_of_the_wildcard_item_does_not_warn() {
    let result = compile(
        "import package::a::{value, *}; fn f() -> u32 { return value(); }",
        &[("package::a", "fn value() -> u32 { return 1; }")],
    )
    .unwrap();
    assert!(result.warnings.is_empty());
}

#[test]
fn local_variables_win_over_wildcard() {
    let out = output(
        "import package::other::*; fn f() -> u32 { let value = 5u; return value; }",
        &[("package::other", "const value: u32 = 1;")],
    );
    assert_eq!(out, "fn f() -> u32 { let value = 5u; return value; }");
}

#[test]
fn wildcard_shadow_can_be_suppressed_on_the_declaration() {
    let w = warnings(
        "import package::other::*; @diagnostic(off, wildcard_shadow) fn value() {} fn f() { value(); }",
        &[("package::other", "fn value() {}")],
    );
    assert!(w.is_empty());
}

#[test]
fn wildcard_shadow_can_be_suppressed_on_the_named_import() {
    let w = warnings(
        "import package::other::*; @diagnostic(off, wildcard_shadow) import package::a::value; fn f() { value(); }",
        &[
            ("package::other", "fn value() {}"),
            ("package::a", "fn value() {}"),
        ],
    );
    assert!(w.is_empty());
}

#[test]
fn wildcard_shadow_can_be_suppressed_module_wide() {
    let w = warnings(
        "import package::other::*; diagnostic(off, wildcard_shadow); fn value() {} fn f() { value(); }",
        &[("package::other", "fn value() {}")],
    );
    assert!(w.is_empty());
}

#[test]
fn wildcard_beats_builtins() {
    let out = output(
        "import package::other::*; fn f() -> f32 { return clamp(1.0); }",
        &[("package::other", "fn clamp(x: f32) -> f32 { return x; }")],
    );
    assert!(out.contains("fn clamp(x: f32) -> f32 { return x; }"));
}

#[test]
fn builtins_are_used_without_a_wildcard_match() {
    let out = output(
        "import package::other::*; fn f() -> f32 { return clamp(1.0, 0.0, 1.0) + helper(); }",
        &[("package::other", "fn helper() -> f32 { return 0.0; }")],
    );
    assert!(out.contains("clamp(1.0, 0.0, 1.0)"));
}

#[test]
fn ambiguous_wildcards_error_where_referenced() {
    let err = usage_error(error(
        "import package::a::*; import package::b::*; fn f() -> u32 { return clashing_zap(); }",
        &[
            ("package::a", "fn clashing_zap() -> u32 { return 1; }"),
            ("package::b", "fn clashing_zap() -> u32 { return 2; }"),
        ],
    ));
    match err {
        UsageError::AmbiguousWildcard { name, candidates } => {
            assert_eq!(name, "clashing_zap");
            assert_eq!(candidates.len(), 2);
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn ambiguous_wildcards_are_fine_when_unreferenced() {
    let out = output(
        "import package::a::*; import package::b::*; fn f() -> u32 { return only_a(); }",
        &[
            (
                "package::a",
                "fn clashing_zap() {} fn only_a() -> u32 { return 1; }",
            ),
            ("package::b", "fn clashing_zap() {}"),
        ],
    );
    assert!(out.contains("only_a"));
}

#[test]
fn ambiguity_is_fixed_by_a_named_import() {
    let out = output(
        "import package::a::*; import package::b::*; import package::b::clashing_zap; fn f() -> u32 { return clashing_zap(); }",
        &[
            ("package::a", "fn clashing_zap() -> u32 { return 1; }"),
            ("package::b", "fn clashing_zap() -> u32 { return 2; }"),
        ],
    );
    assert!(out.contains("return 2;"));
    assert!(!out.contains("return 1;"));
}

#[test]
fn ambiguity_is_fixed_by_an_inline_path() {
    let out = output(
        "import package::a::*; import package::b::*; fn f() -> u32 { return package::a::clashing_zap(); }",
        &[
            ("package::a", "fn clashing_zap() -> u32 { return 1; }"),
            ("package::b", "fn clashing_zap() -> u32 { return 2; }"),
        ],
    );
    assert!(out.contains("return 1;"));
}

#[test]
fn same_declaration_through_two_wildcards_is_not_ambiguous() {
    let out = output(
        "import package::a::*; import package::b::*; fn f() -> u32 { return base_value(); }",
        &[
            ("package::a", "public import package::base::base_value;"),
            ("package::b", "public import package::base::base_value;"),
            (
                "package::base",
                "public fn base_value() -> u32 { return 1; }",
            ),
        ],
    );
    assert_eq!(out.matches("fn base_value").count(), 1);
}

#[test]
fn same_wildcard_twice_is_not_ambiguous() {
    let out = output(
        "import package::a::*; import package::a::{*}; fn f() -> u32 { return value(); }",
        &[("package::a", "fn value() -> u32 { return 1; }")],
    );
    assert_eq!(out.matches("fn value").count(), 1);
}

#[test]
fn reexports_are_imported() {
    let out = output(
        "import package::prelude::*; fn f() -> u32 { return renamed(); }",
        &[
            (
                "package::prelude",
                "public import package::base::value as renamed;",
            ),
            ("package::base", "public fn value() -> u32 { return 1; }"),
        ],
    );
    assert!(out.contains("fn value() -> u32 { return 1; }"));
}

#[test]
fn bare_imports_are_not_reexported() {
    let err = error(
        "import package::prelude::*; fn f() -> u32 { return value(); }",
        &[
            ("package::prelude", "import package::base::value;"),
            ("package::base", "fn value() -> u32 { return 1; }"),
        ],
    );
    assert!(
        err.to_string()
            .contains("cannot find declaration of `value`")
    );
}

#[test]
fn wildcards_are_not_reexported() {
    let err = error(
        "import package::prelude::*; fn f() -> u32 { return value(); }",
        &[
            ("package::prelude", "import package::base::*;"),
            ("package::base", "fn value() -> u32 { return 1; }"),
        ],
    );
    assert!(
        err.to_string()
            .contains("cannot find declaration of `value`")
    );
}

#[test]
fn wildcards_may_be_cyclic() {
    let out = output(
        "import package::a::*; fn f() -> u32 { return from_a(); }",
        &[
            (
                "package::a",
                "import package::b::*; fn from_a() -> u32 { return from_b(); }",
            ),
            (
                "package::b",
                "import package::a::*; fn from_b() -> u32 { return 1; } fn back() -> u32 { return from_a(); }",
            ),
        ],
    );
    assert!(out.contains("fn from_b()"));
}

#[test]
fn unreferenced_wildcard_module_contributes_nothing() {
    let out = output(
        "import package::other::*; fn f() {}",
        &[("package::other", "const_assert 1 < 2; fn value() {}")],
    );
    assert_eq!(out, "fn f() { }");
}

#[test]
fn missing_wildcard_module_is_an_error() {
    let err = error("import package::missing::*; fn f() {}", &[]);
    assert!(err.to_string().contains("missing"));
}

#[test]
fn strip_disabled_keeps_wildcard_items() {
    let mut resolver = VirtualResolver::new();
    resolver.add_module(
        "package".parse().unwrap(),
        "import package::other::*; fn f() -> u32 { return value(); }".into(),
    );
    resolver.add_module(
        "package::other".parse().unwrap(),
        "fn value() -> u32 { return 1; } fn extra() {}".into(),
    );
    let mut compiler = Compiler::default().with_resolver(resolver);
    compiler.options.mangler = ManglerKind::None;
    compiler.options.strip = false;
    let out = squash(&compiler.compile_root().unwrap().to_string());
    assert!(out.contains("fn extra()"));
    assert!(out.contains("fn value()"));
}

#[tokio::test]
async fn compiles_asynchronously() {
    let mut resolver = VirtualResolver::new();
    resolver.add_module(
        "package".parse().unwrap(),
        "import package::other::*; fn f() -> u32 { return value(); }".into(),
    );
    resolver.add_module(
        "package::other".parse().unwrap(),
        "fn value() -> u32 { return 1; }".into(),
    );
    let mut compiler = Compiler::default().with_resolver(resolver);
    compiler.options.mangler = ManglerKind::None;
    compiler.options.keep_main = true;
    let out = squash(&compiler.compile_root_async().await.unwrap().to_string());
    assert_eq!(
        out,
        "fn f() -> u32 { return value(); } fn value() -> u32 { return 1; }"
    );
}

#[test]
fn external_wildcard_requires_wildcardable() {
    let err = usage_error(error(
        "import lib::prelude::*; fn f() { value(); }",
        &[("lib::prelude", "public fn value() {}")],
    ));
    assert!(matches!(err, UsageError::UnsupportedWildcard(_)), "{err}");
}

#[test]
fn external_wildcard_from_wildcardable_module() {
    let out = output(
        "import lib::prelude::*; fn f() { value(); }",
        &[("lib::prelude", "@!wildcardable; public fn value() {}")],
    );
    assert!(out.contains("fn value() {"));
    assert!(!out.contains("wildcardable"));
}

#[test]
fn unsupported_wildcard_can_be_suppressed_on_the_import() {
    let out = output(
        "@diagnostic(off, unsupported_wildcard) import lib::prelude::*; fn f() { value(); }",
        &[("lib::prelude", "public fn value() {}")],
    );
    assert!(out.contains("fn value() {"));
}

#[test]
fn unsupported_wildcard_can_be_suppressed_module_wide() {
    let out = output(
        "import lib::prelude::*; diagnostic(off, unsupported_wildcard); fn f() { value(); }",
        &[("lib::prelude", "public fn value() {}")],
    );
    assert!(out.contains("fn value() {"));
}

#[test]
fn wildcard_from_the_same_package_needs_no_annotation() {
    let out = output(
        "import package::other::*; fn f() { value(); }",
        &[("package::other", "fn value() {}")],
    );
    assert!(out.contains("fn value() {"));
}

#[test]
fn cross_package_wildcard_in_library_code_is_an_error() {
    let err = usage_error(error(
        "import lib::api::entry; fn f() { entry(); }",
        &[
            (
                "lib::api",
                "import other::prelude::*; public fn entry() { value(); }",
            ),
            ("lib/other::prelude", "@!wildcardable; public fn value() {}"),
        ],
    ));
    assert!(matches!(err, UsageError::CrossPackageWildcard(_)), "{err}");
}

#[test]
fn cross_package_wildcard_can_be_suppressed() {
    let out = output(
        "import lib::api::entry; fn f() { entry(); }",
        &[
            (
                "lib::api",
                "@diagnostic(off, cross_package_wildcard) import other::prelude::*; public fn entry() { value(); }",
            ),
            ("lib/other::prelude", "@!wildcardable; public fn value() {}"),
        ],
    );
    assert!(out.contains("fn value() {"));
}

#[test]
fn wildcard_within_a_library_is_allowed() {
    let out = output(
        "import lib::api::entry; fn f() { entry(); }",
        &[
            (
                "lib::api",
                "import package::util::*; public fn entry() { helper(); }",
            ),
            ("lib::util", "fn helper() {}"),
        ],
    );
    assert!(out.contains("fn helper() {"));
}

#[test]
fn builtin_shadow_warns_in_wildcardable_modules() {
    let w = warnings(
        "import lib::prelude::*; fn f() { value(); }",
        &[(
            "lib::prelude",
            "@!wildcardable; public fn value() {} public fn clamp() {}",
        )],
    );
    assert_eq!(
        w,
        vec![Warning::BuiltinShadow {
            module: "lib::prelude".parse().unwrap(),
            name: "clamp".to_string()
        }]
    );
}

#[test]
fn builtin_shadow_can_be_suppressed() {
    let w = warnings(
        "import lib::prelude::*; fn f() { value(); }",
        &[(
            "lib::prelude",
            "@!wildcardable; public fn value() {} @diagnostic(off, builtin_shadow) public fn clamp() {}",
        )],
    );
    assert!(w.is_empty());
}

#[test]
fn builtin_shadow_is_silent_in_other_modules() {
    let w = warnings(
        "import package::other::*; fn f() { value(); }",
        &[("package::other", "fn value() {} fn clamp() {}")],
    );
    assert!(w.is_empty());
}

#[test]
fn conditional_wildcard_import_is_removed() {
    let out = output(
        "@if(false) import package::missing::*; import package::other::*; fn f() { value(); }",
        &[("package::other", "fn value() {}")],
    );
    assert!(out.contains("fn value() {"));
}

#[test]
fn wildcard_items_in_attributes_and_templates() {
    let out = output(
        "import package::consts::*; @compute @workgroup_size(SIZE) fn main() { var a: array<f32, SIZE>; }",
        &[("package::consts", "const SIZE: u32 = 4;")],
    );
    assert!(out.contains("const SIZE: u32 = 4;"));
}

#[test]
fn package_root_can_be_wildcard_imported() {
    let out = output(
        "import package::other::value; fn f() { value(); } fn in_root() {}",
        &[(
            "package::other",
            "import package::*; fn value() { in_root(); }",
        )],
    );
    assert!(out.contains("fn in_root() {"));
}

#[test]
fn visibility_option_makes_private_items_importable() {
    let mut resolver = VirtualResolver::new();
    resolver.add_module(
        "package".parse().unwrap(),
        "import package::other::*; fn f() { secret(); }".into(),
    );
    resolver.add_module(
        "package::other".parse().unwrap(),
        "private fn secret() {}".into(),
    );
    let mut compiler = Compiler::default().with_resolver(resolver);
    compiler.options.keep_main = true;
    compiler.options.visibility = false;
    assert!(compiler.compile_root().is_ok());
}

#[test]
fn library_modules_see_package_visible_items_of_their_package() {
    let out = output(
        "import lib::api::entry; fn f() { entry(); }",
        &[
            (
                "lib::api",
                "import package::util::helper; public fn entry() { helper(); }",
            ),
            ("lib::util", "fn helper() {}"),
        ],
    );
    assert!(out.contains("fn helper() {"));
}

#[test]
fn wildcard_modules_are_listed_as_dependencies() {
    let result = compile(
        "import package::other::*; fn f() {}",
        &[("package::other", "fn value() {}")],
    )
    .unwrap();
    let other = "package::other".parse().unwrap();
    assert!(result.used_items.contains_module(&other));
    assert!(result.used_items.get_module(&other).unwrap().is_empty());
    assert!(!result.modules.iter().any(|module| module.path == other));
}

#[test]
fn module_attributes_are_not_emitted() {
    let out = output(
        "import package::other::*; fn f() { value(); }",
        &[(
            "package::other",
            "@!wildcardable; @!other(1); fn value() {}",
        )],
    );
    assert!(!out.contains('@'));
}

#[test]
fn wesl_diagnostic_rules_are_not_emitted() {
    let out = output(
        "import package::other::*; diagnostic(off, wildcard_shadow); diagnostic(off, derivative_uniformity); @diagnostic(off, wildcard_shadow) fn value() {} @diagnostic(off, derivative_uniformity) fn f() { value(); }",
        &[("package::other", "fn value() {}")],
    );
    assert!(!out.contains("wildcard_shadow"));
    assert!(out.contains("diagnostic (off, derivative_uniformity);"));
    assert!(out.contains("@diagnostic(off, derivative_uniformity) fn f()"));
}

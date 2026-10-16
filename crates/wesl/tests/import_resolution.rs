//! Tests for lazy and eager import resolution.

use wesl::{CompileOptions, CompileResult, Compiler, ImportResolution, resolver::VirtualResolver};

/// Build a compiler over virtual modules, all under the `package` root.
fn compiler(
    resolution: ImportResolution,
    main: &str,
    modules: &[(&str, &str)],
) -> Compiler<VirtualResolver<'static>> {
    let mut resolver = VirtualResolver::new();
    resolver.add_module("package".parse().unwrap(), main.to_string().into());
    for (path, source) in modules {
        resolver.add_module(path.parse().unwrap(), source.to_string().into());
    }
    Compiler::new_with_resolver(
        CompileOptions {
            import_resolution: resolution,
            ..Default::default()
        },
        resolver,
    )
}

/// List the paths of loaded modules, sorted.
fn loaded(result: &CompileResult) -> Vec<String> {
    let mut paths: Vec<_> = result
        .modules
        .iter()
        .map(|module| module.path.to_string())
        .collect();
    paths.sort();
    paths
}

const MAIN: &str = "
import package::types::Light;

fn unused_helper(light: Light) -> f32 {
    return light.intensity;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}
";

const TYPES: &str = "
import package::base::Color;

struct Light {
    color: Color,
    intensity: f32,
}

const_assert 1 < 2;
";

const BASE: &str = "
struct Color {
    rgb: vec3<f32>,
}
";

#[test]
fn lazy_skips_modules_of_unused_declarations() {
    let result = compiler(
        ImportResolution::Lazy,
        MAIN,
        &[("package::types", TYPES), ("package::base", BASE)],
    )
    .compile_root()
    .unwrap();
    assert_eq!(loaded(&result), ["package"]);
}

#[test]
fn eager_loads_modules_of_unused_declarations() {
    let result = compiler(
        ImportResolution::Eager,
        MAIN,
        &[("package::types", TYPES), ("package::base", BASE)],
    )
    .compile_root()
    .unwrap();
    assert_eq!(
        loaded(&result),
        ["package", "package::base", "package::types"]
    );
    assert!(
        result
            .modules
            .iter()
            .find(|module| module.path.to_string() == "package::types")
            .is_some_and(|module| module.syntax.to_string().contains("Light"))
    );
}

#[test]
fn eager_still_strips_unused_declarations() {
    let result = compiler(
        ImportResolution::Eager,
        MAIN,
        &[("package::types", TYPES), ("package::base", BASE)],
    )
    .compile_root()
    .unwrap();
    let wgsl = result.to_string();
    assert!(!wgsl.contains("Light"));
    assert!(!wgsl.contains("Color"));
    assert!(!wgsl.contains("unused_helper"));
    assert!(wgsl.contains("fs_main"));
}

#[test]
fn eager_reports_errors_in_unused_declarations() {
    let main = "
import package::types::Missing;

fn unused_helper(value: Missing) {}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}
";
    let modules = [("package::types", TYPES), ("package::base", BASE)];
    assert!(
        compiler(ImportResolution::Lazy, main, &modules)
            .compile_root()
            .is_ok()
    );
    assert!(
        compiler(ImportResolution::Eager, main, &modules)
            .compile_root()
            .is_err()
    );
}

#[tokio::test]
async fn eager_loads_modules_async() {
    let result = compiler(
        ImportResolution::Eager,
        MAIN,
        &[("package::types", TYPES), ("package::base", BASE)],
    )
    .compile_root_async()
    .await
    .unwrap();
    assert_eq!(
        loaded(&result),
        ["package", "package::base", "package::types"]
    );
}

const ENTRY_ONLY: &str = "
@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}
";

#[test]
fn eager_follows_unused_item_imports() {
    let main = format!("import package::types::Light;\n{ENTRY_ONLY}");
    let modules = [("package::types", TYPES), ("package::base", BASE)];
    let lazy = compiler(ImportResolution::Lazy, &main, &modules)
        .compile_root()
        .unwrap();
    let eager = compiler(ImportResolution::Eager, &main, &modules)
        .compile_root()
        .unwrap();
    assert_eq!(loaded(&lazy), ["package"]);
    assert_eq!(
        loaded(&eager),
        ["package", "package::base", "package::types"]
    );
}

#[test]
fn eager_follows_unused_module_imports() {
    let main = format!("import package::types;\n{ENTRY_ONLY}");
    let modules = [("package::types", TYPES), ("package::base", BASE)];
    let eager = compiler(ImportResolution::Eager, &main, &modules)
        .compile_root()
        .unwrap();
    assert_eq!(
        loaded(&eager),
        ["package", "package::base", "package::types"]
    );
}

#[test]
fn eager_follows_unused_nested_imports() {
    let main = format!("import package::{{types::Light, base}};\n{ENTRY_ONLY}");
    let modules = [("package::types", TYPES), ("package::base", BASE)];
    let eager = compiler(ImportResolution::Eager, &main, &modules)
        .compile_root()
        .unwrap();
    assert_eq!(
        loaded(&eager),
        ["package", "package::base", "package::types"]
    );
}

#[test]
fn eager_reports_unresolved_imports() {
    let modules = [("package::types", TYPES), ("package::base", BASE)];
    for import in ["package::types::Missing", "package::missing"] {
        let main = format!("import {import};\n{ENTRY_ONLY}");
        assert!(
            compiler(ImportResolution::Lazy, &main, &modules)
                .compile_root()
                .is_ok(),
            "lazy {import}"
        );
        assert!(
            compiler(ImportResolution::Eager, &main, &modules)
                .compile_root()
                .is_err(),
            "eager {import}"
        );
    }
}

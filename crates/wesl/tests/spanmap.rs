//! Tests for [`wesl::spanmap`]: mapping naga's byte ranges back to the original sources.

use std::borrow::Cow;

use wesl::{
    CompileOptions, CompileResult, Compiler,
    resolver::VirtualResolver,
    spanmap::{MappedDiagnostic, SpanMap, SpanMapError},
    syntax::ModulePath,
};

const MAIN: &str = "
import package::util::helper;

@compute @workgroup_size(1)
fn main() {
    let r = helper();
}
";

fn compile(files: &[(&str, &str)], options: CompileOptions) -> CompileResult {
    let mut resolver = VirtualResolver::new();
    for (path, src) in files {
        resolver.add_module(
            path.parse::<ModulePath>().unwrap(),
            Cow::Owned(src.to_string()),
        );
    }
    Compiler::new_with_resolver(options, resolver)
        .compile_root()
        .expect("WESL itself does not catch these errors")
}

/// The adapters one writes in a build script to turn naga errors into labels.
fn naga_diagnostic(map: &SpanMap) -> Option<MappedDiagnostic> {
    let module = match naga::front::wgsl::parse_str(map.emitted_source()) {
        Ok(module) => module,
        Err(e) => {
            return Some(
                map.diagnostic(
                    e.message(),
                    e.labels()
                        .filter_map(|(span, msg)| Some((span.to_range()?, msg.to_string()))),
                ),
            );
        }
    };
    let result = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module);
    match result {
        Ok(_) => None,
        Err(e) => Some(
            map.diagnostic_from_error(
                e.as_inner(),
                e.spans()
                    .filter_map(|(span, msg)| Some((span.to_range()?, msg.clone()))),
            ),
        ),
    }
}

fn diagnostic(files: &[(&str, &str)]) -> MappedDiagnostic {
    let result = compile(files, CompileOptions::default());
    let map = result.span_map().unwrap();
    assert_eq!(map.degraded_declarations(), 0);
    naga_diagnostic(&map).expect("naga should reject this program")
}

/// the primary label is the last one.
fn primary(diag: &MappedDiagnostic) -> &wesl::spanmap::SourceLocation {
    diag.labels
        .iter()
        .rev()
        .find_map(|l| l.location.as_ref())
        .expect("a label should resolve to the original source")
}

#[test]
fn parse_error_in_imported_module_points_to_that_module() {
    let util = "
// some padding so offsets in the generated code differ from offsets in this file
public fn helper() -> f32 {
    let x: f32 = 1u;
    return x;
}
";
    let diag = diagnostic(&[("package", MAIN), ("package::util", util)]);
    let loc = primary(&diag);
    // naga points at the definition of `x`: an exact range inside the original file.
    assert_eq!(loc.module.to_string(), "package::util");
    assert_eq!(loc.snippet(), "x");
    assert!(loc.exact);
    assert_eq!((loc.line, loc.column), (4, 9), "{loc}");
    assert_eq!(loc.source(), util);
    assert!(diag.labels[0].text.contains("definition of `x`"));
}

#[test]
fn parse_error_in_main_module_points_to_main() {
    let main = "
@compute @workgroup_size(1)
fn main() {
    let x: f32 = 1u;
}
";
    let diag = diagnostic(&[("package", main)]);
    let loc = primary(&diag);
    assert_eq!(loc.module.to_string(), "package");
    assert_eq!(loc.snippet(), "x");
    assert_eq!((loc.line, loc.column), (4, 9));
}

#[test]
fn validation_error_points_to_the_offending_function() {
    // a function that does not return: reported by the naga validator, not its parser.
    let util = "
public fn helper() -> f32 {
}
";
    let diag = diagnostic(&[("package", MAIN), ("package::util", util)]);
    let loc = primary(&diag);
    assert_eq!(loc.module.to_string(), "package::util");
    assert!(
        loc.snippet().contains("fn helper"),
        "snippet: {:?}",
        loc.snippet()
    );
    assert_eq!(loc.line, 2);
}

#[test]
fn ranges_inside_an_unchanged_node_resolve_exactly() {
    let main = "
@compute @workgroup_size(1)
fn main() {
    let value = 12345u + 1u;
}
";
    let result = compile(&[("package", main)], CompileOptions::default());
    let map = result.span_map().unwrap();
    let start = map.emitted_source().find("12345u").unwrap();
    let loc = map.resolve(start..start + "12345u".len()).unwrap();
    assert!(loc.exact);
    assert_eq!(loc.snippet(), "12345u");
    assert_eq!((loc.line, loc.column), (4, 17));
}

#[test]
fn ranges_in_reformatted_or_renamed_nodes_widen_to_the_node() {
    // the call is printed with different whitespace and a mangled callee name.
    let main = "
import package::util::helper;

@compute @workgroup_size(1)
fn main() {
    let r =   helper(  );
}
";
    let util = "public fn helper() -> f32 { return 1.0; }";
    let result = compile(
        &[("package", main), ("package::util", util)],
        CompileOptions::default(),
    );
    let map = result.span_map().unwrap();
    let start = map.emitted_source().find("package_util_helper()").unwrap();
    let loc = map
        .resolve(start..start + "package_util_helper".len())
        .unwrap();
    assert!(!loc.exact);
    assert_eq!(loc.module.to_string(), "package");
    assert!(
        loc.snippet().contains("helper("),
        "snippet: {:?}",
        loc.snippet()
    );
}

#[test]
fn mangled_names_are_demangled_in_messages() {
    let util = "public fn helper() -> f32 { return 1.0; }";
    let result = compile(
        &[("package", MAIN), ("package::util", util)],
        CompileOptions::default(),
    );
    assert!(result.to_string().contains("package_util_helper"));
    let map = result.span_map().unwrap();
    assert_eq!(
        map.demangle("`package_util_helper` is not a function"),
        "`package::util::helper` is not a function"
    );
    // other identifiers are untouched, even if a mangled name is a prefix of them.
    assert_eq!(
        map.demangle("package_util_helper2 main"),
        "package_util_helper2 main"
    );
}

#[test]
fn rendered_diagnostic_shows_the_original_file_and_line() {
    let util = "
public fn helper() -> f32 {
    let x: f32 = 1u;
    return x;
}
";
    let diag = diagnostic(&[("package", MAIN), ("package::util", util)]);
    let text = diag.render_plain();
    assert!(text.contains("package::util"), "{text}");
    assert!(text.contains("let x: f32 = 1u;"), "{text}");
    // no trace of the mangled name or the generated layout
    assert!(!text.contains("package_util_helper"), "{text}");
    assert!(!text.contains("<generated WGSL>"), "{text}");
}

#[test]
fn unresolvable_labels_fall_back_to_the_generated_code() {
    let result = compile(
        &[
            ("package", MAIN),
            ("package::util", "public fn helper() -> f32 { return 1.0; }"),
        ],
        CompileOptions::default(),
    );
    let map = result.span_map().unwrap();
    // a range past the end of the generated code cannot be resolved.
    let len = map.emitted_source().len();
    let diag = map.diagnostic("oops", [(len + 10..len + 12, "somewhere".to_string())]);
    assert!(diag.labels[0].location.is_none());
    assert!(diag.render_plain().contains("oops"));
}

#[test]
fn span_map_requires_a_sourcemap() {
    let result = compile(
        &[("package", "fn main() {}")],
        CompileOptions {
            sourcemap: false,
            ..Default::default()
        },
    );
    assert!(matches!(result.span_map(), Err(SpanMapError::NoSourcemap)));
}

#[test]
fn stripped_and_unstripped_output_both_map() {
    let util = "
public fn helper() -> f32 {
    let x: f32 = 1u;
    return x;
}
fn unused() {}
";
    for strip in [false, true] {
        let result = compile(
            &[("package", MAIN), ("package::util", util)],
            CompileOptions {
                strip,
                ..Default::default()
            },
        );
        let map = result.span_map().unwrap();
        assert_eq!(map.degraded_declarations(), 0);
        let diag = naga_diagnostic(&map).unwrap();
        assert_eq!(
            primary(&diag).module.to_string(),
            "package::util",
            "strip={strip}"
        );
    }
}

#[test]
fn large_fixture_aligns_node_by_node() {
    // the same program as the `compile_wesl_directory` snapshot test.
    mod package_random {
        use wesl_core::{StaticPackage, StaticPackageModule};
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/package_random.rs"
        ));
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/compile_wesl/shaders");
    for (lower, strip) in [(false, false), (false, true)] {
        let mut compiler = Compiler::default();
        compiler.options.lower = lower;
        compiler.options.strip = strip;
        compiler.options.mangle_main = true;
        let mut constants = wesl::Constants::new();
        constants.set("PI", std::f64::consts::PI);
        constants.set("TRUE", true);
        compiler.options.constants = constants;
        compiler.options.dependencies = vec![&package_random::PACKAGE];
        let result = compiler.compile(&root).unwrap();
        let map = result.span_map().unwrap();
        assert_eq!(
            map.degraded_declarations(),
            0,
            "lower={lower} strip={strip}"
        );

        // every function of the output resolves to a function of the original sources.
        let text = map.emitted_source();
        let mut checked = 0;
        for (i, _) in text.match_indices("\nfn ") {
            let start = i + 1;
            let loc = map
                .resolve(start..start + 2)
                .expect("function should resolve");
            assert!(loc.snippet().contains("fn "), "{loc}: {:?}", loc.snippet());
            checked += 1;
        }
        assert!(checked > 3, "the fixture should have several functions");
    }
}

#[test]
fn causes_of_validation_errors_become_notes() {
    // two resources with the same binding, declared in different files. naga's top-level
    // message does not say what is wrong, the cause is in the chain of sources.
    let main = "
import package::res::light;

@group(0) @binding(0) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(1)
fn main() {
    out[0] = light.x;
}
";
    let res = "
// the same binding as `out` in the main module
@group(0) @binding(0) public var<uniform> light: vec4f;
";
    let diag = diagnostic(&[("package", main), ("package::res", res)]);
    let loc = primary(&diag);
    assert_eq!(loc.module.to_string(), "package::res");
    assert!(
        loc.snippet().contains("var<uniform> light"),
        "{:?}",
        loc.snippet()
    );
    assert_eq!(loc.line, 3);
    assert!(
        diag.notes.iter().any(|n| n.contains("conflict")),
        "notes: {:?}",
        diag.notes
    );
    assert!(diag.render_plain().contains("caused by:"));
}

//! Tests for `public import` in the main module: pipeline-visible items re-exported by the
//! main module are kept, with their exposed names, in the linked output.
//!
//! See <https://github.com/webgpu-tools/wesl-spec/blob/main/Visibility.md#pipeline-visibility>.

use std::borrow::Cow;

use wesl::{CompileOptions, Compiler, error::Error, resolver::VirtualResolver, syntax::ModulePath};

const VERTEX: &str = "
@vertex
public fn vertex_main() -> @builtin(position) vec4f {
    return vec4f(0.0);
}
";

const FRAGMENT: &str = "
fn shade() -> vec4f {
    return vec4f(1.0);
}

@fragment
public fn fragment_main() -> @location(0) vec4f {
    return shade();
}
";

fn resolver(files: &[(&str, &'static str)]) -> VirtualResolver<'static> {
    let mut resolver = VirtualResolver::new();
    for (path, source) in files {
        resolver.add_module(path.parse::<ModulePath>().unwrap(), Cow::Borrowed(source));
    }
    resolver
}

fn compile(options: CompileOptions, files: &[(&str, &'static str)]) -> Result<String, Error> {
    Compiler::new_with_resolver(options, resolver(files))
        .compile_root()
        .map(|res| res.to_string())
}

fn has_fn(output: &str, name: &str) -> bool {
    output.contains(&format!("fn {name}("))
}

/// All combinations of options that affect which main module declarations are kept or renamed.
fn option_matrix() -> Vec<CompileOptions> {
    let mut res = Vec::new();
    for strip in [false, true] {
        for keep_main in [false, true] {
            for mangle_main in [false, true] {
                res.push(CompileOptions {
                    strip,
                    keep_main,
                    mangle_main,
                    ..Default::default()
                });
            }
        }
    }
    res
}

#[test]
fn aggregated_entry_points_are_kept_unmangled() {
    let main = "
        public import package::vertex::vertex_main;
        public import package::fragment::fragment_main;
    ";
    for options in option_matrix() {
        let out = compile(
            options.clone(),
            &[
                ("package", main),
                ("package::vertex", VERTEX),
                ("package::fragment", FRAGMENT),
            ],
        )
        .unwrap();
        assert!(has_fn(&out, "vertex_main"), "{options:?}\n{out}");
        assert!(has_fn(&out, "fragment_main"), "{options:?}\n{out}");
        // helpers are not pipeline-visible: they get mangled.
        assert!(!has_fn(&out, "shade"), "{options:?}\n{out}");
        assert!(out.contains("_shade()"), "{options:?}\n{out}");
    }
}

#[test]
fn public_import_with_main_entry_point() {
    // the issue this was written for: the main module has its own entry point,
    // and re-exports one declared in another module.
    let main = "
        public import package::vertex::vertex_main;

        @compute @workgroup_size(64)
        fn main() {}
    ";
    let out = compile(
        CompileOptions::default(),
        &[("package", main), ("package::vertex", VERTEX)],
    )
    .unwrap();
    assert!(has_fn(&out, "main"), "{out}");
    assert!(has_fn(&out, "vertex_main"), "{out}");
}

#[test]
fn public_import_alias_is_the_exposed_name() {
    let main = "public import package::fragment::fragment_main as fs_main;";
    for options in option_matrix() {
        let out = compile(
            options.clone(),
            &[("package", main), ("package::fragment", FRAGMENT)],
        )
        .unwrap();
        assert!(has_fn(&out, "fs_main"), "{options:?}\n{out}");
        assert!(!out.contains("fragment_main"), "{options:?}\n{out}");
    }
}

#[test]
fn public_import_collection() {
    let main = "public import package::fragment::{fragment_main as fs, shade_all};";
    let fragment = "
        public fn shade_all() {}

        @fragment
        public fn fragment_main() -> @location(0) vec4f { return vec4f(1.0); }
    ";
    let out = compile(
        CompileOptions::default(),
        &[("package", main), ("package::fragment", fragment)],
    )
    .unwrap();
    assert!(has_fn(&out, "fs"), "{out}");
    assert!(has_fn(&out, "shade_all"), "{out}");
}

#[test]
fn public_reexport_chain() {
    // main re-exports an item that `api` itself re-exports from `imp`.
    let main = "public import package::api::run;";
    let api = "public import package::imp::run;";
    let imp = "
        @compute @workgroup_size(1)
        public fn run() {}
    ";
    let out = compile(
        CompileOptions::default(),
        &[
            ("package", main),
            ("package::api", api),
            ("package::imp", imp),
        ],
    )
    .unwrap();
    assert!(has_fn(&out, "run"), "{out}");
}

#[test]
fn resource_variables_and_overrides_keep_their_names() {
    let main = "
        public import package::res::{data, max_lights as lights};
        public import package::res::cs;
    ";
    let res = "
        @group(0) @binding(0) public var<storage, read_write> data: array<f32>;
        public override max_lights: u32;

        @compute @workgroup_size(64)
        public fn cs() {
            data[0] = f32(max_lights);
        }
    ";
    let out = compile(
        CompileOptions::default(),
        &[("package", main), ("package::res", res)],
    )
    .unwrap();
    assert!(out.contains("var<storage, read_write> data:"), "{out}");
    assert!(out.contains("override lights: u32;"), "{out}");
    assert!(out.contains("data[0] = f32(lights)"), "{out}");
    assert!(has_fn(&out, "cs"), "{out}");
}

#[test]
fn bare_import_is_not_reexported() {
    // a bare `import` brings the name in scope without exposing it.
    let main = "import package::vertex::vertex_main;";
    for options in option_matrix() {
        let out = compile(
            options.clone(),
            &[("package", main), ("package::vertex", VERTEX)],
        )
        .unwrap();
        assert!(!out.contains("vertex_main"), "{options:?}\n{out}");
    }
}

#[test]
fn public_import_is_ignored_without_visibility() {
    let main = "public import package::vertex::vertex_main;";
    let out = compile(
        CompileOptions {
            visibility: false,
            ..Default::default()
        },
        &[("package", main), ("package::vertex", VERTEX)],
    )
    .unwrap();
    assert!(!out.contains("vertex_main"), "{out}");
}

#[test]
fn public_import_cannot_widen_visibility() {
    let main = "public import package::vertex::vertex_main;";
    let vertex = "
        @vertex
        fn vertex_main() -> @builtin(position) vec4f { return vec4f(0.0); }
    ";
    let err = compile(
        CompileOptions::default(),
        &[("package", main), ("package::vertex", vertex)],
    )
    .unwrap_err();
    assert!(err.to_string().contains("visibility"), "{err}");
}

#[test]
fn public_import_of_missing_item_is_an_error() {
    let main = "public import package::vertex::nope;";
    let err = compile(
        CompileOptions::default(),
        &[("package", main), ("package::vertex", VERTEX)],
    )
    .unwrap_err();
    assert!(err.to_string().contains("nope"), "{err}");
}

#[test]
fn conflicting_exposed_names_are_an_error() {
    let main = "
        public import package::a::run;
        public import package::b::run;
    ";
    let src = "
        @compute @workgroup_size(1)
        public fn run() {}
    ";
    let err = compile(
        CompileOptions::default(),
        &[("package", main), ("package::a", src), ("package::b", src)],
    )
    .unwrap_err();
    assert!(err.to_string().contains("duplicate"), "{err}");
}

#[test]
fn conflicting_aliases_for_one_item_are_an_error() {
    let main = "
        public import package::vertex::vertex_main as vs1;
        public import package::vertex::vertex_main as vs2;
    ";
    let err = compile(
        CompileOptions::default(),
        &[("package", main), ("package::vertex", VERTEX)],
    )
    .unwrap_err();
    assert!(err.to_string().contains("vs1"), "{err}");
}

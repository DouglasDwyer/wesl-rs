//! WESL compiler frontend.
//!
//! Types and functions in this module are small wrappers that make it easy to use WESL in typical scenarios, "batteries included".

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use wesl_core::StaticPackage;
use wgsl_parse::{
    SyntaxNode,
    syntax::{Ident, ModulePath, PathOrigin, TranslationUnit, Visibility},
};

use crate::{
    SyntaxUtil,
    error::{Diagnostic, Error, ResolveError, UsageError, ValidateError},
    mangler::{self, Mangler},
    pass::{self, CompilerDriver, Features, Module, UsedItems},
    resolver::{Constants, Resolver, StandardResolver},
    sourcemap::{BasicSourceMap, SourceMapper},
};

/// Compilation options used by [`Compiler`].
#[derive(Clone, Debug, PartialEq)]
pub struct CompileOptions {
    /// Toggle [WESL Imports](https://github.com/webgpu-tools/wesl-spec/blob/main/Imports.md).
    ///
    /// If disabled:
    ///
    /// * The compiler will silently remove all import statements and inline paths.
    /// * Validation will not trigger an error if referencing an imported item.
    pub imports: bool,
    /// Toggle [WESL Conditional Translation](https://github.com/webgpu-tools/wesl-spec/blob/main/ConditionalTranslation.md).
    ///
    /// See [`Self::features`] to enable/disable each feature flag.
    pub condcomp: bool,
    /// Toggle [WESL Visibility](https://github.com/webgpu-tools/wesl-spec/blob/main/Visibility.md)
    ///
    /// When disabled, WESL simply ignores `public` and `private` keywords on declarations and treats
    /// all declarations as public.
    pub visibility: bool,
    /// Toggle generics. Generics are super experimental, don't expect anything from it.
    ///
    /// Requires the `generics` crate feature flag.
    pub generics: bool,
    /// Enable stripping (aka. Dead Code Elimination).
    ///
    /// By default, all declarations reachable by entrypoint functions, const_asserts and
    /// pipeline-overridable constants in the main module are kept, as well as items that
    /// the main module re-exports with `public import` (see [`Self::visibility`]).
    /// See [`Self::keep`] and [`Self::keep_main`] to control what gets stripped.
    ///
    /// Stripping can have side-effects: modules are loaded only if statically accessed,
    /// and `const_assert` statements are not always preserved.
    /// Refer to the WESL docs to learn more.
    pub strip: bool,
    /// Enable lowering/polyfills. This transforms the output code in various ways.
    ///
    /// See [`pass::lower`] for the list of transforms.
    pub lower: bool,
    /// Enable validation of individual WESL modules and of the final output.
    ///
    /// This will catch *some* errors, not all.
    /// See [`pass::validate_wesl`] and [`pass::validate_wgsl`] for the list of validations.
    ///
    /// Requires the `eval` crate feature flag.
    pub validate: bool,
    /// Enable sourcemapping, which provides better error diagnostics.
    pub sourcemap: bool,
    /// Sort the declarations of the output with [`TranslationUnit::sort_declarations`].
    ///
    /// This makes the output independent of the order in which modules were linked.
    pub sort_declarations: bool,
    /// Declaration name mangling scheme.
    pub mangler: ManglerKind,
    /// Enable mangling of declarations in the main module.
    ///
    /// By default, WESL does not mangle main module declarations.
    ///
    /// Items re-exported by the main module with `public import` are never mangled: they keep
    /// their imported name, or their `as` alias if renamed.
    pub mangle_main: bool,
    /// If `Some`, specify a list of main module declarations to keep.
    /// If `None`, only the entrypoint functions (and their dependencies) are kept, plus items
    /// re-exported by the main module with `public import`.
    ///
    /// This option has no effect if [`Self::keep_main`] is enabled or  [`Self::strip`] is
    /// disabled.
    pub keep: Option<Vec<String>>,
    /// If `true`, all main module declarations are preserved when stripping is enabled.
    ///
    /// This option takes precedence over [`Self::keep`], and has no effect if
    /// [`Self::strip`] is disabled.
    pub keep_main: bool,
    /// Conditional Translation feature flags.
    ///
    /// This option has no effect if [`Self::condcomp`] is disabled.
    ///
    /// See [`Features`].
    pub features: Features,
    /// Literal constants in the `constants` virtual module.
    ///
    /// See [`Constants`].
    pub constants: Constants,
    /// Importable packages dependencies.
    pub dependencies: Vec<&'static StaticPackage>,
}

/// Declaration name mangling scheme. Used in [`CompileOptions::mangler`].
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManglerKind {
    /// Escaped path mangler.
    /// `foo_bar::item -> _1foo_bar_item`
    #[default]
    Escape,
    /// Hash mangler.
    /// `foo::bar::item -> item_1985638328947`
    Hash,
    /// Make valid identifiers with unicode "confusables" characters.
    /// `foo::bar<baz, moo> -> foo::barᐸbazˏmooᐳ`
    Unicode,
    /// Disable mangling. (warning: will break shaders if case of name conflicts!)
    None,
}

impl From<ManglerKind> for Box<dyn Mangler> {
    fn from(kind: ManglerKind) -> Self {
        match kind {
            ManglerKind::Escape => Box::new(mangler::EscapeMangler),
            ManglerKind::Hash => Box::new(mangler::HashMangler),
            ManglerKind::Unicode => Box::new(mangler::UnicodeMangler),
            ManglerKind::None => Box::new(mangler::NoMangler),
        }
    }
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            imports: true,
            condcomp: true,
            visibility: true,
            generics: false,
            strip: true,
            lower: false,
            validate: true,
            sourcemap: true,
            sort_declarations: false,
            mangler: Default::default(),
            mangle_main: false,
            keep: Default::default(),
            keep_main: false,
            features: Default::default(),
            constants: Default::default(),
            dependencies: Default::default(),
        }
    }
}

/// The WESL compiler.
///
/// # Basic Usage
///
/// ```rust
/// # use wesl::{Compiler, resolver::VirtualResolver};
/// #
/// let compiler = Compiler::default();
/// #
/// # // just adding a virtual file here so the doctest runs without a filesystem
/// # let mut resolver = VirtualResolver::new();
/// # let shader_string = "fn my_fn() {\n\n}\n";
/// # resolver.add_module("package::path::to::shader".parse().unwrap(), shader_string.into());
/// # let mut compiler = compiler.with_resolver(resolver);
/// # compiler.options.keep_main = true; // prevent dead code elimination
/// #
/// let wgsl_string = compiler
///     .compile("path/to/shader.wgsl")
///     .inspect_err(|e| eprintln!("{e}")) // pretty-print errors
///     .unwrap()
///     .to_string();
/// #
/// # assert!(wgsl_string == shader_string);
/// ```
#[derive(Clone, Debug)]
pub struct Compiler<R = ()> {
    pub options: CompileOptions,
    pub resolver: R,
}

impl Default for Compiler<()> {
    fn default() -> Self {
        Self {
            options: Default::default(),
            resolver: Default::default(),
        }
    }
}

impl<R1> Compiler<R1> {
    /// Set the compilation [`crate::Resolver`] or [`crate::AsyncResolver`].
    pub fn with_resolver<R2>(self, resolver: R2) -> Compiler<R2> {
        Compiler::<R2> {
            options: self.options,
            resolver,
        }
    }
}

impl<R> Compiler<R> {
    /// Shorthand for `Compiler::new(options).with_resolver(resolver)`.
    pub fn new_with_resolver(options: CompileOptions, resolver: R) -> Self {
        Self { options, resolver }
    }
}

impl Compiler<()> {
    /// Create a new compiler.
    ///
    /// By default, the compiler will use a [`StandardResolver`] when compiling.
    pub fn new(options: CompileOptions) -> Self {
        let resolver = ();
        Self { options, resolver }
    }
}

/// Standalone version of [`Compiler::compile_module`].
pub fn compile(
    main_path: &ModulePath,
    options: &CompileOptions,
    resolver: &impl Resolver,
) -> Result<CompileResult, Error> {
    let mangler = Box::<dyn Mangler>::from(options.mangler);

    if options.sourcemap {
        let sourcemapper = SourceMapper::new(main_path.clone(), &resolver, &mangler);
        let mut pass = CompilationPass::new(main_path, options, &sourcemapper, &sourcemapper);
        let res = CompilerDriver::compile(&mut pass);
        let sourcemap = sourcemapper.finish();
        let res = res.map_err(|e| Diagnostic::from(e).with_sourcemap(&sourcemap))?;
        let wgsl = res.syntax.to_string().into();

        Ok(CompileResult::new(res, Some(sourcemap), wgsl))
    } else {
        let mut pass = CompilationPass::new(main_path, options, &resolver, &mangler);
        let res = CompilerDriver::compile(&mut pass)?;
        let wgsl = res.syntax.to_string().into();
        Ok(CompileResult::new(res, None, wgsl))
    }
}

/// Async version of [`compile`].
pub async fn compile_async(
    main_path: &ModulePath,
    options: &CompileOptions,
    resolver: &impl Resolver,
) -> Result<CompileResult, Error> {
    let mangler = Box::<dyn Mangler>::from(options.mangler);

    if options.sourcemap {
        let sourcemapper = SourceMapper::new(main_path.clone(), &resolver, &mangler);
        let mut pass = CompilationPass::new(main_path, options, &sourcemapper, &sourcemapper);
        let res = CompilerDriver::compile_async(&mut pass).await;
        let sourcemap = sourcemapper.finish();
        let res = res.map_err(|e| Diagnostic::from(e).with_sourcemap(&sourcemap))?;
        let wgsl = res.syntax.to_string().into();

        Ok(CompileResult::new(res, Some(sourcemap), wgsl))
    } else {
        let mut pass = CompilationPass::new(main_path, options, &resolver, &mangler);
        let res = CompilerDriver::compile_async(&mut pass).await?;
        let wgsl = res.syntax.to_string().into();
        Ok(CompileResult::new(res, None, wgsl))
    }
}

/// Get the module path for a filesystem path relative to the package root directory,
/// with a fallback to the current working directory if the resolver doesn't support
/// filesystem mappings.
///
/// # Panics
///
/// [`ModulePath::from_path`] can panic.
fn main_module_path(path: &Path, resolver: &impl Resolver) -> Result<ModulePath, Error> {
    let mut main_path = resolver.module_path(path).or_else(|e| {
        if matches!(e, ResolveError::FilesystemNotSupported) {
            Ok(ModulePath::from_path(Path::new(".").join(path)))
        } else {
            Err(e)
        }
    })?;

    // we force the origin to be absolute if it was relative.
    main_path.origin = PathOrigin::Absolute;
    Ok(main_path)
}

impl Compiler<()> {
    // TODO: implement and validate wesl-toml semantics.
    /// Compile a WESL shader to WGSL.
    ///
    /// `path` defines where to look for shader files.
    /// It can point to a `wesl.toml` file, a directory or a shader file.
    ///
    /// The main module (which exposes entry points, bindings and overrides) is determined from `path`:
    /// It is the package root module if `path` points to a toml file or a directory,
    /// otherwise it is the file that `path` points to.
    /// See [`Self::compile_module`] to compile a different main module.
    ///
    /// | Path         | Package root directory      | Main module                |
    /// | ------------ | --------------------------- | -------------------------- |
    /// | `wesl.toml`  | `root` field in `wesl.toml` | `package.wesl` in root dir |
    /// | directory    | the directory               | `package.wesl` in root dir |
    /// | `.wesl` file | the parent directory        | the file specified         |
    ///
    /// Note: `.wgsl` extensions are also supported, but `.wesl` takes priority.
    pub fn compile(&self, path: impl AsRef<Path>) -> Result<CompileResult, Error> {
        let (pkg_root_dir, main_path) = self.root_and_main(path.as_ref())?;
        self.compile_module(pkg_root_dir, &main_path)
    }

    /// Variant of [`Self::compile`] with a custom main module path.
    pub fn compile_module(
        &self,
        pkg_root_dir: impl AsRef<Path>,
        main_path: &ModulePath,
    ) -> Result<CompileResult, Error> {
        let resolver = self.create_resolver(pkg_root_dir.as_ref());
        compile(main_path, &self.options, &resolver)
    }

    /// Async version of [`Self::compile`].
    pub async fn compile_async(&self, path: &Path) -> Result<CompileResult, Error> {
        let (pkg_root_dir, main_path) = self.root_and_main(path.as_ref())?;
        self.compile_module_async(&pkg_root_dir, &main_path).await
    }

    /// Async version of [`Self::compile_module`].
    pub async fn compile_module_async(
        &self,
        pkg_root_dir: impl AsRef<Path>,
        main_path: &ModulePath,
    ) -> Result<CompileResult, Error> {
        let resolver = self.create_resolver(pkg_root_dir.as_ref());
        compile_async(main_path, &self.options, &resolver).await
    }

    fn create_resolver(&self, pkg_root_dir: &Path) -> StandardResolver {
        let mut resolver = StandardResolver::new(pkg_root_dir);

        for (name, value) in self.options.constants.iter() {
            resolver.add_constant(name.clone(), *value);
        }

        for package in self.options.dependencies.iter() {
            resolver.add_package(package);
        }

        resolver
    }

    fn root_and_main(&self, path: &Path) -> Result<(PathBuf, ModulePath), Error> {
        let (pkg_root_dir, main_path) = if let Some(file_name) = path.file_name()
            && file_name == "wesl.toml"
        {
            let cfg = crate::toml_cfg::WeslToml::from_file(path)?;
            let root = path
                .parent()
                .unwrap(/* SAFETY: cannot fail if `file_name` succeeds */)
                .join(&cfg.package.root);
            let main = ModulePath::new_root();
            (root, main)
        } else if let Some(name) = path.file_stem()
            && !path.is_dir()
        {
            let root = path.parent().unwrap(/* SAFETY: cannot fail if `file_name` succeeds */).to_path_buf();
            let main = ModulePath::new(
                PathOrigin::Absolute,
                vec![name.to_string_lossy().to_string()],
            );
            (root, main)
        } else {
            let root = path.to_path_buf();
            let main = ModulePath::new_root();
            (root, main)
        };

        Ok((pkg_root_dir, main_path))
    }
}

impl<R: Resolver> Compiler<R> {
    /// Compile a WESL shader to WGSL.
    ///
    /// The main module defaults to the package root module.
    /// See [`Self::compile_module`] to compile a different main module.
    pub fn compile_root(&self) -> Result<CompileResult, Error> {
        compile(&ModulePath::new_root(), &self.options, &self.resolver)
    }

    /// Variant of [`Self::compile`] with a custom main module path.
    pub fn compile_module(&self, main_path: &ModulePath) -> Result<CompileResult, Error> {
        compile(main_path, &self.options, &self.resolver)
    }

    /// Variant of [`Self::compile_root`] with a custom main module path.
    ///
    /// `fs_main_path` defines the main module path according to the resolver's file system mapping implemented in [`Resolver::module_path`].
    ///
    /// # Warning
    ///
    /// This function works best with filesystem resolvers which implement [`Resolver::module_path`].
    /// If not, this function assumes that the package root directory is the current working directory.
    ///
    /// # Panics
    ///
    /// Can panic if [`ModulePath::from_path`] fails.
    // TODO: we don't want that panic.
    pub fn compile(&self, fs_main_path: impl AsRef<Path>) -> Result<CompileResult, Error> {
        let main_path = main_module_path(fs_main_path.as_ref(), &self.resolver)?;
        compile(&main_path, &self.options, &self.resolver)
    }

    /// Async version of [`Self::compile_root`].
    pub async fn compile_root_async(&self) -> Result<CompileResult, Error> {
        compile_async(&ModulePath::new_root(), &self.options, &self.resolver).await
    }

    /// Async version of [`Self::compile_module`].
    pub async fn compile_module_async(
        &self,
        main_path: &ModulePath,
    ) -> Result<CompileResult, Error> {
        compile_async(main_path, &self.options, &self.resolver).await
    }

    /// Async version of [`Self::compile`].
    pub async fn compile_async(
        &self,
        fs_main_path: &impl AsRef<Path>,
    ) -> Result<CompileResult, Error> {
        let main_path = main_module_path(fs_main_path.as_ref(), &self.resolver)?;
        compile_async(&main_path, &self.options, &self.resolver).await
    }
}

/// Result of [`Compiler::compile`].
///
/// This type contains the resulting WGSL syntax tree, the sourcemap (if enabled),
/// and the list of used modules/declarations.
///
/// It implements [`std::fmt::Display`], call `to_string()` to get the compiled WGSL.
#[derive(Default, Clone)]
pub struct CompileResult {
    modules: Vec<Module>,
    sourcemap: Option<BasicSourceMap>,
    syntax: TranslationUnit,
    used_items: UsedItems,
    wgsl: Arc<str>,
}

impl CompileResult {
    /// Emit `rerun-if-changed` instructions so the build script reruns only if the
    /// shader files are modified.
    pub fn emit_rerun_if_changed(&self) {
        let Some(sourcemap) = &self.sourcemap else {
            println!("cargo::warning=cannot emit rerun-if-changed directive without a sourcemap");
            return;
        };

        for (module_path, _) in self.used_items.iter() {
            if module_path.origin.is_package() {
                continue;
            }
            assert!(
                !module_path.origin.is_relative(),
                "the modules passed to emit_rerun_if_changed must be absolute"
            );
            if let Some(source) = sourcemap.file(module_path)
                && let Some(fs_path) = &source.path
            {
                // Path::display is safe here because of the ModulePath naming restrictions
                println!("cargo::rerun-if-changed={}", fs_path.display());

                // If it's a fallback path, we need to react to the higher priority path as well
                if fs_path.extension().unwrap() == "wgsl" {
                    let fs_path = fs_path.with_extension("wesl");
                    println!("cargo::rerun-if-changed={}", fs_path.display());
                }
            }
        }
    }

    /// The modules the syntax tree was linked from.
    pub fn modules(&self) -> &[Module] {
        &self.modules
    }

    /// The sourcemap, if [`CompileOptions::sourcemap`] is enabled.
    pub fn sourcemap(&self) -> Option<&BasicSourceMap> {
        self.sourcemap.as_ref()
    }

    /// The syntax tree of the compiled WGSL.
    pub fn syntax(&self) -> &TranslationUnit {
        &self.syntax
    }

    /// The modules and declarations that the compiled WGSL uses.
    pub fn used_items(&self) -> &UsedItems {
        &self.used_items
    }

    /// The compiled WGSL, printed once when the compilation finished.
    pub fn wgsl(&self) -> &str {
        &self.wgsl
    }

    /// Write the result in rust's `OUT_DIR`.
    ///
    /// This function is meant to be used in a `build.rs` workflow. The output WGSL will
    /// be accessed with the [`wesl_core::include_wesl`] macro. See the crate documentation for a
    /// usage example.
    ///
    /// # Panics
    ///
    /// Panics when the output file cannot be written.
    pub fn write_artifact(&self, artifact_name: &str) {
        let dirname = std::env::var("OUT_DIR").unwrap();
        let out_name = Path::new(artifact_name);
        if out_name.iter().count() != 1 || out_name.extension().is_some() {
            eprintln!("`out_name` cannot contain path separators or file extension");
            panic!()
        }
        let mut output = Path::new(&dirname).join(out_name);
        output.set_extension("wgsl");
        self.write_to_file(output)
            .expect("failed to write output shader");
    }

    /// Write the compiled result to a file.
    pub fn write_to_file(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        std::fs::write(path, self.wgsl())
    }

    /// Creates the result of a compilation, which printed the WGSL `wgsl`.
    fn new(res: pass::CompileResult, sourcemap: Option<BasicSourceMap>, wgsl: Arc<str>) -> Self {
        Self {
            modules: res.modules,
            sourcemap,
            syntax: res.syntax,
            used_items: res.used_items,
            wgsl,
        }
    }
}

impl std::fmt::Display for CompileResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.wgsl)
    }
}

/// Ephemeral type that implements [`CompilerDriver`] for a single compilation pass in [`Compiler::compile`]
struct CompilationPass<'a> {
    main_path: &'a ModulePath,
    options: &'a CompileOptions,
    resolver: &'a dyn Resolver,
    mangler: &'a dyn Mangler,
}

impl<'a> CompilationPass<'a> {
    fn new(
        main_path: &'a ModulePath,
        options: &'a CompileOptions,
        resolver: &'a dyn Resolver,
        mangler: &'a dyn Mangler,
    ) -> Self {
        Self {
            main_path,
            options,
            resolver,
            mangler,
        }
    }

    /// The main module declarations serving as roots, according to [`CompileOptions`].
    fn declared_entry_points(
        &self,
        main_module: &TranslationUnit,
    ) -> Result<HashSet<Ident>, Error> {
        // keep all declarations when strip is disabled or keep_main is enabled.
        if !self.options.strip || self.options.keep_main {
            Ok(main_module
                .global_declarations
                .iter()
                .filter_map(|decl| decl.ident())
                .collect())
        }
        // user provided an explicit list of entry points to start from.
        else if let Some(keep) = &self.options.keep {
            keep.iter()
                .map(|name| {
                    main_module.decl_ident(name).ok_or_else(|| {
                        UsageError::NotFound(self.main_path.clone(), name.to_string()).into()
                    })
                })
                .collect::<Result<HashSet<Ident>, Error>>()
        }
        // otherwise, we keep the WGSL entry points. this is the default.
        else {
            Ok(main_module.entry_points().collect())
        }
    }

    /// Names that `public import`s in the main module expose in the output.
    ///
    /// Pipeline-visible items keep their imported name (or their `as` alias) in the linked
    /// output instead of being mangled.
    ///
    /// Must be called before mangling, because it looks up declarations by name.
    fn exposed_names(&self, modules: &[Module]) -> Result<Vec<(Ident, String)>, Error> {
        let Some(main) = modules.iter().find(|module| module.path == *self.main_path) else {
            return Ok(Vec::new());
        };

        let mut exposed: Vec<(Ident, String)> = Vec::new();
        for (alias, item) in &main.imports {
            if item.visibility != Visibility::Public {
                continue;
            }
            let Some((decl_path, decl_ident)) =
                self.resolve_reexport(modules, &item.path, &item.ident.name())
            else {
                continue;
            };
            if decl_path == *self.main_path {
                continue; // main module declarations are not mangled by default.
            }
            let name = alias.name().to_string();
            match exposed.iter().find(|(ident, _)| *ident == decl_ident) {
                Some((_, other)) if *other == name => {}
                Some((_, other)) => {
                    return Err(Error::Custom(format!(
                        "`{decl_path}::{}` is re-exported by the main module as both `{other}` and `{name}`, which is not supported",
                        decl_ident.name()
                    )));
                }
                None => exposed.push((decl_ident, name)),
            }
        }
        Ok(exposed)
    }

    /// Find the declaration which `name` in module `path` refers to, following re-exports.
    fn resolve_reexport(
        &self,
        modules: &[Module],
        path: &ModulePath,
        name: &str,
    ) -> Option<(ModulePath, Ident)> {
        let mut path = self.resolver.canonical_path(path);
        let mut name = name.to_string();
        // bounded, to be robust against re-export cycles.
        for _ in 0..=modules.len() {
            let module = modules.iter().find(|module| module.path == path)?;
            let decl = module
                .syntax
                .global_declarations
                .iter()
                .filter_map(|decl| decl.ident())
                .find(|ident| *ident.name() == name);
            if let Some(ident) = decl {
                return Some((path, ident));
            }
            let (_, item) = module
                .imports
                .iter()
                .find(|(ident, _)| *ident.name() == name)?;
            path = self.resolver.canonical_path(&item.path);
            name = item.ident.name().to_string();
        }
        None
    }
}

impl CompilerDriver for CompilationPass<'_> {
    fn main_path(&self) -> &ModulePath {
        self.main_path
    }

    fn canonical_path(&self, path: &ModulePath) -> ModulePath {
        self.resolver.canonical_path(path)
    }

    fn main_entry_points(&self, main_module: &TranslationUnit) -> Result<HashSet<Ident>, Error> {
        let mut roots = self.declared_entry_points(main_module)?;
        // items re-exported by the main module with `public import` are part of the
        // pipeline-visible API: they are roots of static usage analysis, even though
        // nothing in the main module references them.
        if self.options.visibility {
            roots.extend(
                pass::flatten_imports(&main_module.imports, self.main_path)
                    .into_iter()
                    .filter(|(_, item)| item.visibility == Visibility::Public)
                    .map(|(ident, _)| ident),
            );
        }
        Ok(roots)
    }

    fn module_usage_analysis(
        &self,
        module: &Module,
        already_used: &mut UsedItems,
        to_analyze: &mut UsedItems,
    ) -> Result<(), Error> {
        pass::module_usage_analysis(module, already_used, to_analyze, !self.options.visibility)?;

        // when strip is disabled, all declarations in the module are included so they
        // must be usage-analyzed.
        if !self.options.strip {
            for decl in &module.syntax.global_declarations {
                if let Some(ident) = decl.ident() {
                    self.usage_analysis(
                        module,
                        &ident.name(),
                        decl.visibility(),
                        already_used,
                        to_analyze,
                    )?;
                }
            }
        }

        Ok(())
    }

    fn usage_analysis(
        &self,
        module: &Module,
        decl_name: &str,
        min_vis: Visibility,
        already_used: &mut UsedItems,
        to_analyze: &mut UsedItems,
    ) -> Result<(), Error> {
        pass::usage_analysis(
            module,
            decl_name,
            min_vis,
            already_used,
            to_analyze,
            !self.options.visibility,
        )?;
        Ok(())
    }

    fn load_module(&mut self, path: &ModulePath) -> Result<TranslationUnit, Error> {
        let mut module = pass::load_module(path, &self.resolver)?;

        if self.options.condcomp {
            pass::condcomp(&mut module, &self.options.features)?;
        }

        pass::retarget_idents(&mut module);

        if self.options.validate {
            pass::validate_wesl(&module)?;
        }

        Ok(module)
    }

    fn link(
        &self,
        modules: &mut Vec<Module>,
        used_items: &UsedItems,
    ) -> Result<TranslationUnit, Error> {
        pass::retarget_modules(modules, used_items, &self.resolver);

        let exposed = if self.options.visibility {
            self.exposed_names(modules)?
        } else {
            Vec::new()
        };

        for module in modules.iter_mut() {
            if !self.options.mangle_main && module.path == *self.main_path {
                continue;
            }
            pass::mangle(&mut module.syntax, &module.path, &self.mangler);
        }

        // pipeline-visible items re-exported by the main module keep their exposed names.
        // idents are shared, so renaming a declaration also renames all references to it.
        if !exposed.is_empty() {
            let mut taken = modules
                .iter()
                .flat_map(|module| module.syntax.global_declarations.iter())
                .filter_map(|decl| decl.ident())
                .filter(|ident| !exposed.iter().any(|(exposed, _)| exposed == ident))
                .map(|ident| ident.name().to_string())
                .collect::<HashSet<_>>();
            for (_, name) in &exposed {
                if !taken.insert(name.clone()) {
                    return Err(ValidateError::Duplicate(name.clone()).into());
                }
            }
            for (mut ident, name) in exposed {
                ident.rename(name);
            }
        }

        let mut module = pass::link(modules, self.options.strip.then_some(used_items));

        if self.options.lower {
            pass::lower(&mut module)?;
        }

        if self.options.validate {
            pass::validate_wgsl(&module)?;
        }

        if self.options.sort_declarations {
            module.sort_declarations();
        }

        Ok(module)
    }
}

#[test]
fn test_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Compiler<()>>();
}
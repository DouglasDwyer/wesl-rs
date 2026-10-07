//! [`SourceMap`] trait and implementations.

use std::{cell::RefCell, collections::HashMap, ops::Range, path::PathBuf};

use wgsl_parse::{
    span::Span,
    syntax::{TranslationUnit, TypeExpression},
};

pub use crate::spanmap::{MappedDiagnostic, MappedLabel, SourceLocation};
use crate::{
    ModulePath, error::ResolveError, mangler::Mangler, pass::Module, resolver::Resolver, spanmap,
    spanmap::Output,
};

/// A SourceMap is a lookup from compiled WGSL to source WESL. It translates a mangled
/// name into a module path and declaration name.
///
/// Using SourceMaps improves the readability of error diagnostics, by providing needed
/// information to identify the originating code snippet, file name and declaration name.
/// It is highly recommended to use them, but they can increase the compilation memory
/// footprint, since they cache all loaded files.
///
/// Typically you record to a SourceMap by passing a [`SourceMapper`] as the [`Resolver`]
/// and [`Mangler`] when compiling code.
pub trait SourceMap {
    /// Get the module path and declaration name from a mangled name.
    fn item(&self, decl: &str) -> Option<&SourceMapEntry>;
    /// Get a module contents.
    fn source(&self, path: &ModulePath) -> Option<&str>;
    /// Get a module display name.
    fn display_name(&self, path: &ModulePath) -> Option<&str>;
    /// Get the default module contents.
    fn default_source(&self) -> Option<&str> {
        None
    }
    /// Replaces the mangled names in `text` by the paths they come from.
    fn demangle_message(&self, text: &str) -> String {
        let is_separator = |c: char| !(c.is_alphanumeric() || c == '_');
        text.split_inclusive(is_separator)
            .map(|piece| {
                let (word, separator) =
                    piece.split_at(piece.find(is_separator).unwrap_or(piece.len()));
                match self.item(word).filter(|entry| entry.name != word) {
                    Some(entry) => format!("{}::{}{separator}", entry.path, entry.name),
                    None => piece.to_string(),
                }
            })
            .collect()
    }
    /// Translates a byte range of the compiled code to the innermost syntax node that contains it.
    ///
    /// The range is exact if the node was printed as written, and widened to the node otherwise.
    /// Returns `None` unless the map was finished with the compiled code.
    fn destination_to_source(&self, _span: Range<usize>) -> Option<SourceLocation> {
        None
    }
    /// Creates a diagnostic from a message and labeled ranges of the compiled code.
    ///
    /// The last label is the primary one, as in the validation errors of naga.
    fn diagnostic(
        &self,
        message: impl AsRef<str>,
        labels: impl IntoIterator<Item = (Range<usize>, String)>,
    ) -> MappedDiagnostic
    where
        Self: Sized,
    {
        spanmap::diagnostic(self, message, labels)
    }
    /// Like [`Self::diagnostic`], with the chain of causes of `error` added as notes.
    fn diagnostic_from_error(
        &self,
        error: &(dyn std::error::Error + 'static),
        labels: impl IntoIterator<Item = (Range<usize>, String)>,
    ) -> MappedDiagnostic
    where
        Self: Sized,
    {
        spanmap::diagnostic_from_error(self, error, labels)
    }
}

#[derive(Clone, Debug)]
pub struct SourceMapEntry {
    pub path: ModulePath,
    pub name: String,
    pub span: Option<Span>,
}

#[derive(Clone, Debug)]
pub struct SourceMapFile {
    pub source: String,
    pub display_name: Option<String>,
    pub path: Option<PathBuf>,
}

/// Basic implementation of [`SourceMap`].
#[derive(Clone, Debug, Default)]
pub struct BasicSourceMap {
    mappings: HashMap<String, SourceMapEntry>,
    sources: HashMap<ModulePath, SourceMapFile>,
    default_source: Option<String>,
    pub(crate) output: Option<Output>,
}

impl BasicSourceMap {
    pub fn new() -> Self {
        Default::default()
    }
    pub fn add_item(&mut self, decl: String, entry: SourceMapEntry) {
        self.mappings.insert(decl, entry);
    }
    /// Iterates over the mangled names and the declarations they come from.
    pub fn items(&self) -> impl Iterator<Item = (&str, &SourceMapEntry)> {
        self.mappings.iter().map(|(k, v)| (k.as_str(), v))
    }
    pub fn file(&self, path: &ModulePath) -> Option<&SourceMapFile> {
        self.sources.get(path)
    }
    pub fn add_file(&mut self, path: ModulePath, file: SourceMapFile) {
        self.sources.insert(path, file);
    }
    pub fn set_default_source(&mut self, source: String) {
        self.default_source = Some(source);
    }
}

impl SourceMap for BasicSourceMap {
    fn item(&self, decl: &str) -> Option<&SourceMapEntry> {
        self.mappings.get(decl)
    }
    fn source(&self, path: &ModulePath) -> Option<&str> {
        self.sources.get(path).map(|file| file.source.as_str())
    }
    fn display_name(&self, path: &ModulePath) -> Option<&str> {
        self.sources
            .get(path)
            .and_then(|file| file.display_name.as_deref())
    }
    fn default_source(&self) -> Option<&str> {
        self.default_source.as_deref()
    }
    fn destination_to_source(&self, span: Range<usize>) -> Option<SourceLocation> {
        spanmap::destination_to_source(self, span)
    }
}

impl<T: SourceMap> SourceMap for Option<T> {
    fn item(&self, decl: &str) -> Option<&SourceMapEntry> {
        self.as_ref().and_then(|map| map.item(decl))
    }
    fn source(&self, path: &ModulePath) -> Option<&str> {
        self.as_ref().and_then(|map| map.source(path))
    }
    fn display_name(&self, path: &ModulePath) -> Option<&str> {
        self.as_ref().and_then(|map| map.display_name(path))
    }
    fn default_source(&self) -> Option<&str> {
        self.as_ref().and_then(|map| map.default_source())
    }
    fn destination_to_source(&self, span: Range<usize>) -> Option<SourceLocation> {
        self.as_ref()
            .and_then(|map| map.destination_to_source(span))
    }
}

/// This [`SourceMap`] implementation simply does nothing and returns `None`.
///
/// It can be useful to pass this struct to functions requiring a source map, but
/// you don't care about source mapping.
pub struct NoSourceMap;

impl SourceMap for NoSourceMap {
    fn item(&self, _decl: &str) -> Option<&SourceMapEntry> {
        None
    }
    fn source(&self, _path: &ModulePath) -> Option<&str> {
        None
    }
    fn display_name(&self, _path: &ModulePath) -> Option<&str> {
        None
    }
    fn default_source(&self) -> Option<&str> {
        None
    }
}

/// Generate a SourceMap by keeping track of loaded files and mangled identifiers.
///
/// `SourceMapper` is a proxy that implements [`Mangler`] and [`Resolver`]. To record a
/// SourceMap, invoke the compiler with this instance as both the mangler and the
/// resolver. Call [`SourceMapper::finish`] to get the final SourceMap once finished
/// recording.
pub struct SourceMapper<'a> {
    pub main_path: ModulePath,
    pub resolver: &'a dyn Resolver,
    pub mangler: &'a dyn Mangler,
    pub source_map: RefCell<BasicSourceMap>,
}

impl<'a> SourceMapper<'a> {
    /// Create a new `SourceMapper` from a mangler and a resolver.
    pub fn new(
        main_path: ModulePath,
        resolver: &'a dyn Resolver,
        mangler: &'a dyn Mangler,
    ) -> Self {
        Self {
            main_path,
            resolver,
            mangler,
            source_map: Default::default(),
        }
    }
    /// Consume this and return a [`BasicSourceMap`].
    pub fn finish(self) -> BasicSourceMap {
        let mut source_map = self.source_map.into_inner();
        if let Some(file) = source_map.file(&self.main_path) {
            source_map.set_default_source(file.source.to_string());
        }
        source_map
    }
    /// Like [`Self::finish`], and records the compiled code of `syntax` for [`BasicSourceMap::destination_to_source`].
    ///
    /// `modules` are the modules `syntax` was linked from.
    pub fn finish_with_output(
        self,
        syntax: &TranslationUnit,
        modules: &[Module],
    ) -> BasicSourceMap {
        let mut source_map = self.finish();
        source_map.output = Some(Output::new(syntax, modules, &source_map));
        source_map
    }
}

impl<'a> Resolver for SourceMapper<'a> {
    fn resolve_source(&self, path: &ModulePath) -> Result<std::borrow::Cow<'a, str>, ResolveError> {
        let res = self.resolver.resolve_source(path)?;
        let mut source_map = self.source_map.borrow_mut();
        source_map.add_file(
            path.clone(),
            SourceMapFile {
                source: res.clone().into(),
                display_name: self.resolver.display_name(path),
                path: self.resolver.fs_path(path).ok(),
            },
        );
        Ok(res)
    }
    fn display_name(&self, path: &ModulePath) -> Option<String> {
        self.resolver.display_name(path)
    }
    fn fs_path(&self, path: &ModulePath) -> Result<PathBuf, ResolveError> {
        self.resolver.fs_path(path)
    }
    fn canonical_path(&self, path: &ModulePath) -> ModulePath {
        self.resolver.canonical_path(path)
    }
}

impl<'a> Mangler for SourceMapper<'a> {
    fn mangle(&self, path: &ModulePath, item: &str) -> String {
        let res = self.mangler.mangle(path, item);
        let mut source_map = self.source_map.borrow_mut();
        let entry = SourceMapEntry {
            path: path.clone(),
            name: item.to_string(),
            span: None,
        };
        source_map.add_item(res.clone(), entry);
        res
    }
    fn unmangle(&self, mangled: &str) -> Option<(ModulePath, String)> {
        self.mangler.unmangle(mangled)
    }
    fn mangle_types(&self, item: &str, variant: u32, types: &[TypeExpression]) -> String {
        self.mangler.mangle_types(item, variant, types)
    }
}

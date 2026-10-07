//! Map byte ranges in the compiled WGSL back to the original WESL source files.
//!
//! WESL links all modules into a single WGSL string. Tools that consume that string (e.g.
//! naga) report errors as byte ranges into the *generated* text, which has been reformatted
//! and has mangled names. A [`SpanMap`] translates those ranges back to the file, line and
//! snippet the author actually wrote, so errors can be rendered against the original code.
//!
//! This module does not depend on any particular version of naga: it works on plain byte
//! ranges. Converting a naga error into ranges takes a few lines, see [`SpanMap::diagnostic_from_error`].
//!
//! ```ignore
//! let result = compiler.compile("src/shaders")?;
//! let map = result.span_map()?;
//!
//! // IMPORTANT: parse exactly the text the map was built from.
//! let module = naga::front::wgsl::parse_str(map.emitted_source()).map_err(|e| {
//!     map.diagnostic_from_error(
//!         &e,
//!         e.labels().filter_map(|(span, msg)| Some((span.to_range()?, msg.to_string()))),
//!     )
//! })?;
//! ```
//!
//! # How it works
//!
//! The final syntax tree still holds the spans of every node, relative to the module it
//! came from. We print the tree, parse the printed text back, and walk both trees in
//! parallel: they are isomorphic, so the n-th node of one is the n-th node of the other.
//! That gives, for every declaration, statement and expression, its range in the generated
//! text and its range in the original file.
//!
//! # Precision
//!
//! Only declarations, statements and expressions carry spans. A range reported inside one
//! of them resolves to the innermost enclosing node. When that node was printed exactly as
//! it was written (the common case for small expressions) the range is mapped precisely,
//! otherwise it resolves to the whole node.

use std::{
    collections::HashMap,
    fmt::{self, Display},
    ops::Range,
    sync::Arc,
};

use wgsl_parse::{
    SyntaxNode,
    syntax::{ExpressionNode, GlobalDeclaration, ModulePath, StatementNode, TranslationUnit},
};

use crate::{
    CompileResult,
    pass::Visit,
    sourcemap::{BasicSourceMap, SourceMap},
};

/// Error building a [`SpanMap`].
#[derive(Clone, Debug, thiserror::Error)]
pub enum SpanMapError {
    /// The compilation was run with `CompileOptions::sourcemap` disabled, so the original
    /// sources are not available.
    #[error("cannot build a span map without a sourcemap: enable `CompileOptions::sourcemap`")]
    NoSourcemap,
    /// The generated WGSL could not be parsed back. This is a bug in WESL's output.
    #[error("the generated WGSL cannot be parsed back: {0}")]
    Reparse(String),
}

/// A location in an original source file.
#[derive(Clone, Debug)]
pub struct SourceLocation {
    /// Path of the module the range is in.
    pub module: ModulePath,
    /// Human-readable file name (usually a file path).
    pub file: String,
    /// Byte range in the original source.
    pub span: Range<usize>,
    /// 1-based line number of the start of the range.
    pub line: usize,
    /// 1-based column (in characters) of the start of the range.
    pub column: usize,
    /// `true` if the range is exact; `false` if it was widened to an enclosing syntax node.
    pub exact: bool,
    source: Arc<str>,
}

impl SourceLocation {
    /// The full text of the original file.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The original text covered by [`Self::span`].
    pub fn snippet(&self) -> &str {
        self.source.get(self.span.clone()).unwrap_or_default()
    }
}

impl Display for SourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

/// A node of the generated WGSL with its counterpart in the original source.
#[derive(Clone, Debug)]
struct Node {
    emitted: Range<usize>,
    orig: Range<usize>,
    file: usize,
    /// The generated text of this node is identical to the original text.
    exact: bool,
}

struct File {
    module: ModulePath,
    name: String,
    source: Arc<str>,
}

/// A lookup from byte ranges in the generated WGSL to the original sources.
///
/// See the [module documentation](self).
pub struct SpanMap {
    emitted: Arc<str>,
    nodes: Vec<Node>,
    files: Vec<File>,
    /// mangled name -> original path (`module::name`)
    names: HashMap<String, String>,
    /// number of declarations for which only declaration-level precision is available.
    degraded: usize,
}

impl fmt::Debug for SpanMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpanMap")
            .field("nodes", &self.nodes.len())
            .field("files", &self.files.len())
            .field("degraded", &self.degraded)
            .finish()
    }
}

fn collect_exprs(decl: &GlobalDeclaration) -> Vec<&ExpressionNode> {
    fn rec<'a>(expr: &'a ExpressionNode, out: &mut Vec<&'a ExpressionNode>) {
        out.push(expr);
        for child in Visit::<ExpressionNode>::visit(expr.node()) {
            rec(child, out);
        }
    }
    let mut out = Vec::new();
    for expr in Visit::<ExpressionNode>::visit(decl) {
        rec(expr, &mut out);
    }
    out
}

fn collect_stmts(decl: &GlobalDeclaration) -> Vec<&StatementNode> {
    fn rec<'a>(stmt: &'a StatementNode, out: &mut Vec<&'a StatementNode>) {
        out.push(stmt);
        for child in Visit::<StatementNode>::visit(stmt.node()) {
            rec(child, out);
        }
    }
    let mut out = Vec::new();
    for stmt in Visit::<StatementNode>::visit(decl) {
        rec(stmt, &mut out);
    }
    out
}

/// Shrink a range so that it does not include leading or trailing whitespace.
fn trim_range(text: &str, range: Range<usize>) -> Range<usize> {
    let Some(slice) = text.get(range.clone()) else {
        return range;
    };
    let start = range.start + (slice.len() - slice.trim_start().len());
    let end = range.end - (slice.len() - slice.trim_end().len());
    start..end.max(start)
}

fn file_index(
    files: &mut Vec<File>,
    sourcemap: &BasicSourceMap,
    path: &ModulePath,
) -> Option<usize> {
    if let Some(i) = files.iter().position(|f| f.module == *path) {
        return Some(i);
    }
    let source: Arc<str> = sourcemap.source(path)?.into();
    let name = sourcemap
        .display_name(path)
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string());
    files.push(File {
        module: path.clone(),
        name,
        source,
    });
    Some(files.len() - 1)
}

impl CompileResult {
    /// Build a [`SpanMap`] to translate ranges of `self.to_string()` back to the original
    /// WESL sources.
    ///
    /// Requires `CompileOptions::sourcemap` (enabled by default).
    pub fn span_map(&self) -> Result<SpanMap, SpanMapError> {
        let sourcemap = self.sourcemap.as_ref().ok_or(SpanMapError::NoSourcemap)?;
        let emitted = self.syntax.to_string();
        let reparsed: TranslationUnit = emitted
            .parse()
            .map_err(|e: wgsl_parse::Error| SpanMapError::Reparse(e.to_string()))?;

        let mut files: Vec<File> = Vec::new();
        let mut nodes = Vec::new();
        let mut degraded = 0;

        // `Display` does not print void declarations.
        let orig_decls = self
            .syntax
            .global_declarations
            .iter()
            .filter(|decl| !matches!(decl.node(), GlobalDeclaration::Void));

        for (orig, new) in orig_decls.zip(reparsed.global_declarations.iter()) {
            // Which module does this declaration come from? Declarations are cloned when
            // linking, but their identifiers are shared, so we can compare them by address.
            let module =
                self.modules.iter().find(|module| {
                    module.syntax.global_declarations.iter().any(|d| {
                        match (d.ident(), orig.ident()) {
                            (Some(a), Some(b)) => a == b,
                            (None, None) => d.span() == orig.span() && d == orig,
                            _ => false,
                        }
                    })
                });
            let Some(file) = module.and_then(|m| file_index(&mut files, sourcemap, &m.path)) else {
                degraded += 1;
                continue;
            };
            let source = files[file].source.clone();

            // pairs of (range in generated text, range in original source)
            let mut pairs = vec![(new.span().range(), orig.span().range())];
            let (orig_exprs, new_exprs) = (collect_exprs(orig.node()), collect_exprs(new.node()));
            let (orig_stmts, new_stmts) = (collect_stmts(orig.node()), collect_stmts(new.node()));
            if orig_exprs.len() == new_exprs.len() && orig_stmts.len() == new_stmts.len() {
                pairs.extend(
                    new_exprs
                        .iter()
                        .zip(&orig_exprs)
                        .map(|(n, o)| (n.span().range(), o.span().range())),
                );
                pairs.extend(
                    new_stmts
                        .iter()
                        .zip(&orig_stmts)
                        .map(|(n, o)| (n.span().range(), o.span().range())),
                );
            } else {
                // the trees diverge: only keep the declaration itself.
                degraded += 1;
            }

            for (e, o) in pairs {
                let (e, o) = (trim_range(&emitted, e), trim_range(&source, o));
                let (Some(e_text), Some(o_text)) = (emitted.get(e.clone()), source.get(o.clone()))
                else {
                    continue;
                };
                nodes.push(Node {
                    exact: e_text == o_text,
                    emitted: e,
                    orig: o,
                    file,
                });
            }
        }

        let names = sourcemap
            .items()
            .filter(|(mangled, entry)| *mangled != entry.name)
            .map(|(mangled, entry)| {
                (
                    mangled.to_string(),
                    format!("{}::{}", entry.path, entry.name),
                )
            })
            .collect();

        Ok(SpanMap {
            emitted: emitted.into(),
            nodes,
            files,
            names,
            degraded,
        })
    }
}

fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let before = source.get(..offset).unwrap_or(source);
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}

impl SpanMap {
    /// The generated WGSL this map was built from.
    ///
    /// Pass exactly this text to the tool whose ranges you want to translate. It is the
    /// same as `CompileResult::to_string()`.
    pub fn emitted_source(&self) -> &str {
        &self.emitted
    }

    /// The number of declarations for which only declaration-level precision is available,
    /// because the generated code could not be aligned node by node with the original.
    /// This should be zero.
    pub fn degraded_declarations(&self) -> usize {
        self.degraded
    }

    /// Translate a byte range of [`Self::emitted_source`] to the original source.
    ///
    /// The range resolves to the innermost declaration, statement or expression that
    /// contains it. Returns `None` if the range is not inside any known node (e.g. a
    /// directive).
    pub fn resolve(&self, range: Range<usize>) -> Option<SourceLocation> {
        let node = self
            .nodes
            .iter()
            .filter(|n| n.emitted.start <= range.start && range.end <= n.emitted.end)
            .min_by_key(|n| n.emitted.len())?;
        let file = &self.files[node.file];

        // if the node was printed exactly as written, we can map the sub-range precisely.
        let precise = node.exact.then(|| {
            let start = node.orig.start + (range.start - node.emitted.start);
            let end = node.orig.start + (range.end - node.emitted.start);
            (start..end, true)
        });
        let (span, exact) = match precise {
            Some((span, exact)) if file.source.get(span.clone()).is_some() => (span, exact),
            _ => (node.orig.clone(), false),
        };

        let (line, column) = line_col(&file.source, span.start);
        Some(SourceLocation {
            module: file.module.clone(),
            file: file.name.clone(),
            span,
            line,
            column,
            exact,
            source: file.source.clone(),
        })
    }

    /// Replace mangled names in a message by the path of the declaration they come from,
    /// e.g. `package_util_helper` becomes `package::util::helper`.
    pub fn demangle(&self, text: &str) -> String {
        fn flush(map: &SpanMap, token: &mut String, out: &mut String) {
            match map.names.get(token.as_str()) {
                Some(original) => out.push_str(original),
                None => out.push_str(token),
            }
            token.clear();
        }

        let mut out = String::with_capacity(text.len());
        let mut token = String::new();
        for c in text.chars() {
            if c.is_alphanumeric() || c == '_' {
                token.push(c);
            } else {
                flush(self, &mut token, &mut out);
                out.push(c);
            }
        }
        flush(self, &mut token, &mut out);
        out
    }

    /// Create a diagnostic that points to the original source.
    ///
    /// `labels` are ranges of [`Self::emitted_source`] with a description. For tools that
    /// report several labels for one error (such as naga's validation errors, which list
    /// the enclosing function first and the offending expression last), the *last* label is
    /// highlighted as the primary one.
    ///
    /// With naga:
    ///
    /// ```ignore
    /// // parse errors
    /// map.diagnostic_from_error(
    ///     &e,
    ///     e.labels().filter_map(|(span, msg)| Some((span.to_range()?, msg.to_string()))),
    /// )
    /// // validation errors
    /// map.diagnostic_from_error(
    ///     e.as_inner(),
    ///     e.spans().filter_map(|(span, msg)| Some((span.to_range()?, msg.clone()))),
    /// )
    /// ```
    ///
    /// Prefer [`Self::diagnostic_from_error`], which also includes the chain of causes.
    pub fn diagnostic(
        &self,
        message: impl AsRef<str>,
        labels: impl IntoIterator<Item = (Range<usize>, String)>,
    ) -> MappedDiagnostic {
        MappedDiagnostic {
            message: self.demangle(message.as_ref()),
            labels: labels
                .into_iter()
                .map(|(emitted, text)| MappedLabel {
                    text: self.demangle(&text),
                    location: self.resolve(emitted.clone()),
                    emitted,
                })
                .collect(),
            notes: Vec::new(),
            emitted: self.emitted.clone(),
        }
    }
}

impl SpanMap {
    /// Like [`Self::diagnostic`], taking the message from an error and adding its chain of
    /// causes (`Error::source`) as notes.
    ///
    /// Many errors only say *where* they happened in their top-level message, e.g. naga's
    /// "Entry point main at Compute is invalid", and keep the actual reason in the chain.
    pub fn diagnostic_from_error(
        &self,
        error: &(dyn std::error::Error + 'static),
        labels: impl IntoIterator<Item = (Range<usize>, String)>,
    ) -> MappedDiagnostic {
        let mut diagnostic = self.diagnostic(error.to_string(), labels);
        let mut source = error.source();
        while let Some(cause) = source {
            diagnostic
                .notes
                .push(format!("caused by: {}", self.demangle(&cause.to_string())));
            source = cause.source();
        }
        diagnostic
    }
}

/// A label of a [`MappedDiagnostic`].
#[derive(Clone, Debug)]
pub struct MappedLabel {
    /// The description of the label.
    pub text: String,
    /// The range in the generated WGSL, as reported by the tool.
    pub emitted: Range<usize>,
    /// The location in the original source, if it could be resolved.
    pub location: Option<SourceLocation>,
}

/// An error whose labels were translated to the original sources by a [`SpanMap`].
///
/// It implements [`Display`], rendering a Rust-compiler-like snippet per original file.
/// Labels that could not be resolved are rendered against the generated WGSL.
#[derive(Clone, Debug)]
pub struct MappedDiagnostic {
    pub message: String,
    pub labels: Vec<MappedLabel>,
    pub notes: Vec<String>,
    emitted: Arc<str>,
}

impl MappedDiagnostic {
    /// Add a note displayed after the snippets, e.g. the causes of a validation error.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    fn render(&self, renderer: &annotate_snippets::Renderer) -> String {
        use annotate_snippets::*;

        let title = Level::ERROR.primary_title(&self.message);
        let mut group = Group::with_title(title);
        let last = self.labels.len().saturating_sub(1);
        let kind = |i: usize| {
            if i == last {
                AnnotationKind::Primary
            } else {
                AnnotationKind::Context
            }
        };

        // one snippet per original file, in order of first appearance.
        let mut modules: Vec<&ModulePath> = Vec::new();
        for loc in self.labels.iter().filter_map(|l| l.location.as_ref()) {
            if !modules.contains(&&loc.module) {
                modules.push(&loc.module);
            }
        }
        for module in modules {
            let labels: Vec<(usize, &MappedLabel, &SourceLocation)> = self
                .labels
                .iter()
                .enumerate()
                .filter_map(|(i, l)| {
                    let loc = l.location.as_ref()?;
                    (loc.module == *module).then_some((i, l, loc))
                })
                .collect();
            let first = labels[0].2;
            let mut snippet = Snippet::source(first.source()).path(&first.file).fold(true);
            for (i, label, loc) in labels {
                let mut annotation = kind(i).span(loc.span.clone());
                if !label.text.is_empty() {
                    annotation = annotation.label(&label.text);
                }
                snippet = snippet.annotation(annotation);
            }
            group = group.element(snippet);
        }

        // labels we could not resolve are shown against the generated code.
        let unresolved: Vec<(usize, &MappedLabel)> = self
            .labels
            .iter()
            .enumerate()
            .filter(|(_, l)| l.location.is_none() && l.emitted.end <= self.emitted.len())
            .collect();
        if !unresolved.is_empty() {
            let mut snippet = Snippet::source(&*self.emitted)
                .path("<generated WGSL>")
                .fold(true);
            for (i, label) in unresolved {
                let mut annotation = kind(i).span(label.emitted.clone());
                if !label.text.is_empty() {
                    annotation = annotation.label(&label.text);
                }
                snippet = snippet.annotation(annotation);
            }
            group = group.element(snippet);
        }

        for note in &self.notes {
            group = group.element(Level::NOTE.message(note));
        }

        renderer.render(&[group])
    }

    /// Render without colors.
    pub fn render_plain(&self) -> String {
        self.render(&annotate_snippets::Renderer::plain())
    }

    /// Render with ANSI colors.
    pub fn render_colored(&self) -> String {
        self.render(&annotate_snippets::Renderer::styled())
    }
}

impl Display for MappedDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.render_plain())
    }
}

impl std::error::Error for MappedDiagnostic {}

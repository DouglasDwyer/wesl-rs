//! Maps byte ranges of the compiled WGSL back to the original WESL sources.
//!
//! The queries are provided by [`SourceMap`], which returns the types of this module.

use std::{
    fmt::{self, Display},
    ops::Range,
    sync::Arc,
};

use wgsl_parse::{
    PrintedSpan, SyntaxNode, print_with_spans,
    syntax::{GlobalDeclaration, GlobalDeclarationNode, ModulePath, TranslationUnit},
};

use crate::{
    pass::Module,
    source_map::{BasicSourceMap, SourceMap},
};

/// An error whose labels point at the original sources.
#[derive(Clone, Debug)]
pub struct MappedDiagnostic {
    /// The labeled ranges, the last one being the primary one.
    pub labels: Vec<MappedLabel>,
    /// The message of the error.
    pub message: String,
    /// The notes displayed after the snippets.
    pub notes: Vec<String>,
}

/// A label of a [`MappedDiagnostic`].
#[derive(Clone, Debug)]
pub struct MappedLabel {
    /// The range in the compiled code, as reported by the tool.
    pub emitted: Range<usize>,
    /// The location in the original source, if the range could be resolved.
    pub location: Option<SourceLocation>,
    /// The description of the label.
    pub text: String,
}

/// A syntax node found in both the compiled code and an original source.
#[derive(Clone, Debug)]
pub(crate) struct Node {
    /// The range of the node in the compiled code.
    pub emitted: Range<usize>,
    /// Whether the compiled text of the node equals its original text.
    pub exact: bool,
    /// The index of the module the node comes from in [`Output::files`].
    pub file: usize,
    /// The range of the node in the original source.
    pub orig: Range<usize>,
}

/// The compiled code and the nodes it was printed from, recorded when a source_map is finished.
#[derive(Clone, Debug)]
pub(crate) struct Output {
    /// The compiled code the nodes were recorded from.
    pub emitted: Arc<str>,
    /// The modules that contributed code.
    pub files: Vec<ModulePath>,
    /// The nodes of the compiled code, children before their parents.
    pub nodes: Vec<Node>,
}

/// A location in an original source file.
#[derive(Clone, Debug)]
pub struct SourceLocation {
    /// The 1-based column, in characters, the range starts at.
    pub column: usize,
    /// Whether the range is exact, or was widened to the enclosing syntax node.
    pub exact: bool,
    /// The name of the file, usually its path.
    pub file: String,
    /// The 1-based line the range starts on.
    pub line: usize,
    /// The module the location is in.
    pub module: ModulePath,
    /// The full text of the original source.
    source: Arc<str>,
    /// The byte range in the original source.
    pub span: Range<usize>,
}

/// Creates an annotation of `span` with `text` as its label, if there is one.
fn annotate(
    kind: annotate_snippets::AnnotationKind,
    span: Range<usize>,
    text: &str,
) -> annotate_snippets::Annotation<'_> {
    let annotation = kind.span(span);
    if text.is_empty() {
        annotation
    } else {
        annotation.label(text)
    }
}

/// Returns the index of the module `path` in `files`, adding it if its source is known.
fn file_index(
    files: &mut Vec<ModulePath>,
    source_map: &BasicSourceMap,
    path: &ModulePath,
) -> Option<usize> {
    if let Some(index) = files.iter().position(|file| file == path) {
        return Some(index);
    }
    source_map.source(path)?;
    files.push(path.clone());
    Some(files.len() - 1)
}

/// Returns whether `linked` is the linked copy of `declaration`.
fn is_copy_of(linked: &GlobalDeclarationNode, declaration: &GlobalDeclarationNode) -> bool {
    match (linked.ident(), declaration.ident()) {
        (Some(a), Some(b)) => a == b,
        (None, None) => linked.span() == declaration.span() && linked == declaration,
        _ => false,
    }
}

/// Returns the 1-based line and column of the byte `offset` in `source`.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let before = source.get(..offset).unwrap_or(source);
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}

/// Builds the node for a printed span, if its file and original text are known.
fn node(
    printed: &PrintedSpan,
    regions: &[(Range<usize>, usize)],
    files: &[ModulePath],
    source_map: &BasicSourceMap,
    emitted: &str,
) -> Option<Node> {
    let (_, file) = regions
        .iter()
        .find(|(range, _)| range.start <= printed.range.start && printed.range.end <= range.end)?;
    if printed.span.range().is_empty() {
        return None;
    }
    let orig_text = source_map
        .source(&files[*file])?
        .get(printed.span.range())?;
    Some(Node {
        emitted: printed.range.clone(),
        exact: emitted.get(printed.range.clone()) == Some(orig_text),
        file: *file,
        orig: printed.span.range(),
    })
}

/// Translates a byte range of the compiled code to the innermost node of `map` that contains it.
pub(crate) fn destination_to_source(
    map: &BasicSourceMap,
    span: Range<usize>,
) -> Option<SourceLocation> {
    let output = map.output.as_ref()?;
    let node = output
        .nodes
        .iter()
        .filter(|n| n.emitted.start <= span.start && span.end <= n.emitted.end)
        .min_by_key(|n| n.emitted.len())?;
    let module = &output.files[node.file];
    let source = map.source(module)?;

    let shifted = node.exact.then(|| {
        let offset = span.start - node.emitted.start;
        node.orig.start + offset..node.orig.start + offset + span.len()
    });
    let (range, exact) = match shifted {
        Some(range) if source.get(range.clone()).is_some() => (range, true),
        _ => (node.orig.clone(), false),
    };

    let (line, column) = line_col(source, range.start);
    Some(SourceLocation {
        column,
        exact,
        file: map
            .display_name(module)
            .map_or_else(|| module.to_string(), str::to_string),
        line,
        module: module.clone(),
        source: source.into(),
        span: range,
    })
}

/// Creates a diagnostic from a message and labeled ranges of the code compiled with `map`.
pub(crate) fn diagnostic(
    map: &impl SourceMap,
    message: impl AsRef<str>,
    labels: impl IntoIterator<Item = (Range<usize>, String)>,
) -> MappedDiagnostic {
    MappedDiagnostic {
        labels: labels
            .into_iter()
            .map(|(emitted, text)| MappedLabel {
                location: map.destination_to_source(emitted.clone()),
                emitted,
                text: map.demangle_message(&text),
            })
            .collect(),
        message: map.demangle_message(message.as_ref()),
        notes: Vec::new(),
    }
}

/// Like [`diagnostic`], with the chain of causes of `error` added as notes.
pub(crate) fn diagnostic_from_error(
    map: &impl SourceMap,
    error: &(dyn std::error::Error + 'static),
    labels: impl IntoIterator<Item = (Range<usize>, String)>,
) -> MappedDiagnostic {
    let mut diagnostic = diagnostic(map, error.to_string(), labels);
    let mut source = error.source();
    while let Some(cause) = source {
        let note = format!("caused by: {}", map.demangle_message(&cause.to_string()));
        diagnostic.notes.push(note);
        source = cause.source();
    }
    diagnostic
}

impl MappedDiagnostic {
    /// Renders the diagnostic with `renderer`, with one snippet per original file.
    fn render(&self, renderer: &annotate_snippets::Renderer) -> String {
        use annotate_snippets::*;

        let last = self.labels.len().saturating_sub(1);
        let kind = |index: usize| match index == last {
            true => AnnotationKind::Primary,
            false => AnnotationKind::Context,
        };
        let title = Level::ERROR.primary_title(&self.message);
        let mut group = Group::with_title(title);

        let mut modules: Vec<&ModulePath> = Vec::new();
        for location in self.labels.iter().filter_map(|l| l.location.as_ref()) {
            if !modules.contains(&&location.module) {
                modules.push(&location.module);
            }
        }
        for module in modules {
            let labels = self.labels.iter().enumerate().filter_map(|(i, label)| {
                let location = label.location.as_ref().filter(|l| l.module == *module)?;
                Some((i, label, location))
            });
            let labels = labels.collect::<Vec<_>>();
            let first = labels[0].2;
            let mut snippet = Snippet::source(first.source()).path(&first.file).fold(true);
            for (index, label, location) in labels {
                let annotation = annotate(kind(index), location.span.clone(), &label.text);
                snippet = snippet.annotation(annotation);
            }
            group = group.element(snippet);
        }

        for label in self.labels.iter().filter(|l| l.location.is_none()) {
            let text = format!("{} (generated code {:?})", label.text, label.emitted);
            group = group.element(Level::NOTE.message(text));
        }

        for note in &self.notes {
            group = group.element(Level::NOTE.message(note));
        }
        renderer.render(&[group])
    }

    /// Renders the diagnostic with ANSI colors.
    pub fn render_colored(&self) -> String {
        self.render(&annotate_snippets::Renderer::styled())
    }

    /// Renders the diagnostic without colors.
    pub fn render_plain(&self) -> String {
        self.render(&annotate_snippets::Renderer::plain())
    }

    /// Adds a note displayed after the snippets.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

impl Display for MappedDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.render_plain())
    }
}

impl std::error::Error for MappedDiagnostic {}

impl Output {
    /// Prints `syntax` and records where the nodes of each module in `modules` ended up.
    pub(crate) fn new(
        syntax: &TranslationUnit,
        modules: &[Module],
        source_map: &BasicSourceMap,
    ) -> Self {
        let (emitted, printed) = print_with_spans(syntax);

        let declarations = syntax
            .global_declarations
            .iter()
            .filter(|decl| !matches!(decl.node(), GlobalDeclaration::Void))
            .collect::<Vec<_>>();
        let roots = printed.iter().filter(|p| p.depth == 0).collect::<Vec<_>>();
        let roots = &roots[roots.len().saturating_sub(declarations.len())..];

        let mut files = Vec::new();
        let mut regions: Vec<(Range<usize>, usize)> = Vec::new();
        for (root, declaration) in roots.iter().zip(&declarations) {
            let module = modules.iter().find(|module| {
                let linked = &module.syntax.global_declarations;
                linked.iter().any(|decl| is_copy_of(decl, declaration))
            });
            let file = module.and_then(|m| file_index(&mut files, source_map, &m.path));
            if let (Some(file), true) = (file, root.span == declaration.span()) {
                regions.push((root.range.clone(), file));
            }
        }

        let nodes = printed
            .iter()
            .filter_map(|p| node(p, &regions, &files, source_map, &emitted))
            .collect();
        Self {
            emitted: emitted.into(),
            files,
            nodes,
        }
    }
}

impl SourceLocation {
    /// The text of the original source covered by [`Self::span`].
    pub fn snippet(&self) -> &str {
        self.source.get(self.span.clone()).unwrap_or_default()
    }

    /// The full text of the original source file.
    pub fn source(&self) -> &str {
        &self.source
    }
}

impl Display for SourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

use std::{
    fmt::{self, Write as _},
    mem,
    ops::Range,
};

use crate::span::Span;

/// A syntax node that can be printed through a [`SyntaxWriter`].
pub trait Print {
    /// Writes the node to `w`.
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result;
}

/// A spanned node whose text is still being written.
struct OpenSpan {
    /// The span of the node in its source.
    pub span: Span,
    /// The position of the first character of the node, once it is written.
    pub start: Option<usize>,
}

/// A spanned syntax node and the range of printed text it produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintedSpan {
    /// The number of spanned nodes enclosing this one.
    pub depth: usize,
    /// The byte range of the node in the printed text, without surrounding whitespace.
    pub range: Range<usize>,
    /// The span of the node in the source it was parsed from.
    pub span: Span,
}

/// Writes printed syntax and can record where each spanned node ends up in the text.
///
/// Newlines inside an indented scope are held back until more text follows, so that the
/// last newline of the scope can be dropped.
pub struct SyntaxWriter<'a> {
    /// The number of nested indented scopes.
    depth: usize,
    /// Whether a semicolon was held back and not yet sent to the sink.
    held_semicolon: bool,
    /// Whether the indentation of the current line was written.
    line_started: bool,
    /// The spanned nodes currently being printed, outermost first.
    open: Vec<OpenSpan>,
    /// Whether a newline was written by the caller but not yet sent to the sink.
    pending_newline: bool,
    /// The number of bytes written to the sink.
    position: usize,
    /// The destination of the printed text.
    sink: &'a mut dyn fmt::Write,
    /// The recorded spans, or `None` if spans are not recorded.
    spans: Option<Vec<PrintedSpan>>,
    /// Whether a trailing semicolon is currently held back.
    trim_semicolon: bool,
    /// The number of writes so far, used to tell if a scope wrote anything.
    writes: usize,
}

impl<'a> SyntaxWriter<'a> {
    /// Writes text that contains no newline.
    fn content(&mut self, text: &str) -> fmt::Result {
        if text.is_empty() {
            return Ok(());
        }
        self.start_line()?;
        self.writes += 1;
        let trimmed = self
            .trim_semicolon
            .then(|| text.strip_suffix(';'))
            .flatten();
        let body = trimmed.unwrap_or(text);
        if !body.is_empty() {
            self.mark_started();
            self.put(body)?;
        }
        self.held_semicolon = trimmed.is_some();
        Ok(())
    }

    /// Sends the held semicolon to the sink.
    fn flush_semicolon(&mut self) -> fmt::Result {
        if self.held_semicolon {
            self.held_semicolon = false;
            self.put(";")?;
        }
        Ok(())
    }

    /// Runs `f` with every line it writes indented one level deeper.
    ///
    /// The final newline written by `f` is dropped.
    pub fn indented<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let writes = self.writes;
        self.depth += 1;
        let result = f(self);
        self.depth -= 1;
        if self.writes != writes && self.pending_newline {
            self.pending_newline = false;
            self.line_started = true;
        }
        result
    }

    /// Returns the recorded spans, ordered by when their nodes finished printing.
    pub fn into_spans(self) -> Vec<PrintedSpan> {
        self.spans.unwrap_or_default()
    }

    /// Prints `items` separated by `separator`.
    pub fn join<'i, T: Print + 'i>(
        &mut self,
        items: impl IntoIterator<Item = &'i T>,
        separator: &str,
    ) -> fmt::Result {
        self.join_by(items, separator, |w, item| w.print(item))
    }

    /// Prints `items` with `print`, separated by `separator`.
    pub fn join_by<I>(
        &mut self,
        items: impl IntoIterator<Item = I>,
        separator: &str,
        mut print: impl FnMut(&mut Self, I) -> fmt::Result,
    ) -> fmt::Result {
        for (i, item) in items.into_iter().enumerate() {
            if 0 < i {
                self.write_str(separator)?;
            }
            print(self, item)?;
        }
        Ok(())
    }

    /// Sets the start of the open spans that have not written any text yet.
    fn mark_started(&mut self) {
        for open in self.open.iter_mut().rev() {
            if open.start.is_some() {
                break;
            }
            open.start = Some(self.position);
        }
    }

    /// Writes a newline, holding it back inside an indented scope.
    fn newline(&mut self) -> fmt::Result {
        self.start_line()?;
        self.writes += 1;
        self.line_started = false;
        if 0 < self.depth {
            self.pending_newline = true;
            Ok(())
        } else {
            self.put("\n")
        }
    }

    /// Prints `node`.
    pub fn print<T: Print + ?Sized>(&mut self, node: &T) -> fmt::Result {
        node.print(self)
    }

    /// Sends `text` to the sink.
    fn put(&mut self, text: &str) -> fmt::Result {
        self.sink.write_str(text)?;
        self.position += text.len();
        Ok(())
    }

    /// Flushes held text and writes the indentation of the current line.
    fn start_line(&mut self) -> fmt::Result {
        self.flush_semicolon()?;
        if self.pending_newline {
            self.pending_newline = false;
            self.put("\n")?;
        }
        if !self.line_started {
            self.line_started = true;
            for _ in 0..self.depth {
                self.put("    ")?;
            }
        }
        Ok(())
    }

    /// Runs `f` and associates the text it writes with `span`.
    pub fn with_span<R>(&mut self, span: Span, f: impl FnOnce(&mut Self) -> R) -> R {
        if self.spans.is_none() {
            return f(self);
        }
        let depth = self.open.len();
        self.open.push(OpenSpan { span, start: None });
        let result = f(self);
        let open = self
            .open
            .pop()
            .expect("spans are opened and closed in pairs");
        if let (Some(start), Some(spans)) = (open.start, self.spans.as_mut()) {
            spans.push(PrintedSpan {
                span: open.span,
                range: start..self.position,
                depth,
            });
        }
        result
    }

    /// Runs `f` and drops the semicolon it writes last, if any.
    pub fn without_trailing_semicolon<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let outer = mem::replace(&mut self.trim_semicolon, true);
        let result = f(self);
        self.held_semicolon = false;
        self.trim_semicolon = outer;
        result
    }

    /// Creates a writer that prints to `sink` without recording spans.
    pub fn new(sink: &'a mut dyn fmt::Write) -> Self {
        Self {
            depth: 0,
            held_semicolon: false,
            line_started: false,
            open: Vec::new(),
            pending_newline: false,
            position: 0,
            sink,
            spans: None,
            trim_semicolon: false,
            writes: 0,
        }
    }

    /// Creates a writer that prints to `sink` and records the range of every spanned node.
    pub fn recording(sink: &'a mut dyn fmt::Write) -> Self {
        Self {
            spans: Some(Vec::new()),
            ..Self::new(sink)
        }
    }
}

impl fmt::Write for SyntaxWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut lines = s.split('\n');
        if let Some(first) = lines.next() {
            self.content(first)?;
        }
        for line in lines {
            self.newline()?;
            self.content(line)?;
        }
        Ok(())
    }
}

/// Prints `node` and returns the text with the range of every spanned node in it.
pub fn print_with_spans<T: Print + ?Sized>(node: &T) -> (String, Vec<PrintedSpan>) {
    let mut text = String::new();
    let mut writer = SyntaxWriter::recording(&mut text);
    node.print(&mut writer)
        .expect("writing to a string cannot fail");
    let spans = writer.into_spans();
    (text, spans)
}

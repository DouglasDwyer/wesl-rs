//! Tests for printing syntax through a `SyntaxWriter`.

use std::fmt::Write as _;

use wgsl_parse::{
    PrintedSpan, SyntaxWriter, parse_str, print_with_spans, span::Span, syntax::TranslationUnit,
};

const KITCHEN_SINK: &str = "
enable f16;

alias Float = f32;

struct Light {
    @size(16) position: vec3f,
    color: vec3f,
}

const_assert 1 < 2;

@group(0) @binding(0) var<uniform> light: Light;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    var total = 0.0;
    for (var i = 0; i < 4; i++) {
        if i == 2 {
            continue;
        } else if i == 3 {
            break;
        } else {
            total += light.color[i];
        }
    }
    loop {
        total -= 1.0;
        continuing {
            break if total < 0.0;
        }
    }
    switch id.x {
        case 0u, 1u: {
            discard;
        }
        default: {
            while total > 0.0 {
                total = total - 1.0;
            }
        }
    }
}

fn helper(a: f32, b: f32) -> f32 {
    return select(a, b, a > b);
}
";

fn parse(source: &str) -> TranslationUnit {
    parse_str(source).expect("the test source should parse")
}

fn printed(source: &str) -> (String, Vec<PrintedSpan>) {
    print_with_spans(&parse(source))
}

fn find<'a>(text: &str, spans: &'a [PrintedSpan], node: &str) -> &'a PrintedSpan {
    spans
        .iter()
        .find(|s| &text[s.range.clone()] == node)
        .unwrap_or_else(|| panic!("no node printed as {node:?}"))
}

#[test]
fn recording_does_not_change_the_printed_text() {
    let tu = parse(KITCHEN_SINK);
    assert_eq!(print_with_spans(&tu).0, tu.to_string());
}

#[test]
fn blocks_are_indented_and_end_without_a_trailing_newline() {
    let (text, _) = printed("fn f() { var a = 1; { a = 2; } }");
    assert_eq!(
        text,
        "fn f() {\n    var a = 1;\n    {\n        a = 2;\n    }\n}\n"
    );
}

#[test]
fn empty_blocks_print_a_blank_line() {
    let (text, _) = printed("fn f() {}");
    assert_eq!(text, "fn f() {\n\n}\n");
}

#[test]
fn blank_lines_in_nested_blocks_keep_their_indentation() {
    let (text, _) = printed("fn f() { if true { } }");
    assert_eq!(text, "fn f() {\n    if true {\n    \n    }\n}\n");
}

#[test]
fn for_headers_drop_the_semicolons_of_their_statements() {
    let (text, spans) = printed("fn f() { for (var i = 0; i < 4; i++) { } }");
    assert!(text.contains("for (var i = 0; i < 4; i++) {"), "{text}");
    assert_eq!(find(&text, &spans, "var i = 0").depth, 2);
    assert_eq!(find(&text, &spans, "i++").depth, 2);
}

#[test]
fn spans_cover_exactly_the_printed_nodes() {
    let source = "fn f(a: i32) -> i32 {\n  let b =   a + 1;\n\n  return b;\n}";
    let (text, spans) = printed(source);
    let function = "fn f(a: i32) -> i32 {\n    let b = a + 1;\n    return b;\n}";
    assert_eq!(text, format!("{function}\n"));

    assert_eq!(find(&text, &spans, function).depth, 0);
    assert_eq!(find(&text, &spans, "let b = a + 1;").depth, 1);
    assert_eq!(find(&text, &spans, "return b;").depth, 1);
    assert_eq!(find(&text, &spans, "a + 1").depth, 2);
}

#[test]
fn spans_are_the_ones_of_the_parsed_source() {
    let source = "fn f(a: i32) -> i32 {\n  let b =   a + 1;\n\n  return b;\n}";
    let (text, spans) = printed(source);
    for (node, original) in [
        ("let b = a + 1;", "let b =   a + 1;"),
        ("return b;", "return b;"),
        ("a + 1", "a + 1"),
    ] {
        let span = find(&text, &spans, node).span;
        assert_eq!(&source[span.range()], original);
    }
}

#[test]
fn nodes_are_recorded_after_their_children() {
    let (text, spans) = printed("fn f() { return 1 + 2; }");
    let position = |node: &str| {
        let wanted = find(&text, &spans, node);
        spans.iter().position(|s| s == wanted).unwrap()
    };
    assert!(position("1") < position("1 + 2"));
    assert!(position("1 + 2") < position("return 1 + 2;"));
}

#[test]
fn attributes_and_struct_members_are_recorded() {
    let (text, spans) = printed("struct S { @size(16) a: f32, b: f32 }");
    assert_eq!(find(&text, &spans, "@size(16)").depth, 2);
    assert_eq!(find(&text, &spans, "@size(16)\n    a: f32").depth, 1);
    assert_eq!(find(&text, &spans, "b: f32").depth, 1);
}

#[test]
fn every_recorded_range_is_inside_the_text_and_trimmed() {
    let (text, spans) = printed(KITCHEN_SINK);
    assert_eq!(spans.iter().filter(|s| s.depth == 0).count(), 6);
    for span in spans {
        let node = text.get(span.range.clone()).expect("range is in the text");
        assert!(!node.is_empty());
        assert_eq!(node, node.trim(), "{:?}", span.range);
    }
}

fn written(f: impl FnOnce(&mut SyntaxWriter<'_>)) -> String {
    let mut text = String::new();
    f(&mut SyntaxWriter::new(&mut text));
    text
}

#[test]
fn indented_scopes_drop_their_final_newline() {
    let text = written(|w| {
        w.write_str("{\n").unwrap();
        w.indented(|w| w.write_str("a\nb\n")).unwrap();
        w.write_str("\n}").unwrap();
    });
    assert_eq!(text, "{\n    a\n    b\n}");
}

#[test]
fn indented_scopes_nest() {
    let text = written(|w| {
        w.write_str("0\n").unwrap();
        w.indented(|w| {
            w.write_str("1\n").unwrap();
            w.indented(|w| w.write_str("2")).unwrap();
            w.write_str("\n1")
        })
        .unwrap();
        w.write_str("\n0").unwrap();
    });
    assert_eq!(text, "0\n    1\n        2\n    1\n0");
}

#[test]
fn empty_indented_scopes_keep_the_newline_before_them() {
    let text = written(|w| {
        w.write_str("{\n").unwrap();
        w.indented(|_| ());
        w.write_str("\n}").unwrap();
    });
    assert_eq!(text, "{\n\n}");
}

#[test]
fn trailing_semicolons_are_dropped_only_at_the_end() {
    let text = written(|w| {
        w.without_trailing_semicolon(|w| w.write_str("a;b;"))
            .unwrap();
        w.write_str("c;").unwrap();
    });
    assert_eq!(text, "a;bc;");
}

#[test]
fn held_semicolons_are_written_if_text_follows_in_the_scope() {
    let text = written(|w| {
        w.without_trailing_semicolon(|w| {
            w.write_str("a;").unwrap();
            w.write_str("b;").unwrap();
        });
    });
    assert_eq!(text, "a;b");
}

#[test]
fn recorded_spans_report_ranges_and_depths() {
    let outer = Span::new(0..10);
    let inner = Span::new(2..4);
    let mut text = String::new();
    let mut w = SyntaxWriter::recording(&mut text);
    w.with_span(outer, |w| {
        w.write_str("ab").unwrap();
        w.with_span(inner, |w| w.write_str("cd")).unwrap();
        w.write_str("ef")
    })
    .unwrap();
    assert_eq!(
        w.into_spans(),
        [
            PrintedSpan {
                span: inner,
                range: 2..4,
                depth: 1
            },
            PrintedSpan {
                span: outer,
                range: 0..6,
                depth: 0
            },
        ]
    );
}

#[test]
fn spans_without_text_are_not_recorded() {
    let mut text = String::new();
    let mut w = SyntaxWriter::recording(&mut text);
    w.with_span(Span::new(0..1), |_| ());
    assert!(w.into_spans().is_empty());
}

#[test]
fn plain_writers_record_nothing() {
    let mut text = String::new();
    let mut w = SyntaxWriter::new(&mut text);
    w.with_span(Span::new(0..1), |w| w.write_str("a")).unwrap();
    assert!(w.into_spans().is_empty());
    assert_eq!(text, "a");
}

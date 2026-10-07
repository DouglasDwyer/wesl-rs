use std::str::FromStr;

use crate::{
    error::Error,
    lexer::{Lexer, Token, TokenIterator},
    syntax::*,
};

#[allow(clippy::all, reason = "generated code")]
mod grammar {
    include!("grammar.rs");
}

pub use crate::parser_support::ParseEntryPoint;
use grammar::EntryPointParser;

macro_rules! parse {
    ($source:expr, $token:ident, $entrypoint:ident) => {{
        match parse_tokens(Lexer::new($source), Token::$token) {
            Ok(ParseEntryPoint::$entrypoint(res)) => Ok(res),
            Ok(_) => unreachable!("parser parsed the wrong entrypoint"),
            Err(e) => Err(e),
        }
    }};
}

/// Parse a token stream into a syntax tree.
///
/// Low-level implementation, you probably don't want to use this. See [`parse_str`].
///
/// The `entrypoint` parameter must be one of the `EntryPointXXX` tokens, which tells the
/// parser which syntax node to expect. It returns the [`ParseEntryPoint`] union type.
pub fn parse_tokens(
    lexer: impl TokenIterator,
    entrypoint: Token,
) -> Result<ParseEntryPoint, Error> {
    let lexer = std::iter::once(Ok((0, entrypoint, 0))).chain(lexer);
    let parser = EntryPointParser::new();
    parser.parse(lexer).map_err(Into::into)
}

/// Parse a string into a syntax tree ([`TranslationUnit`]).
///
/// Identical to [`TranslationUnit::from_str`].
pub fn parse_str(source: &str) -> Result<TranslationUnit, Error> {
    parse!(source, EntryPointTranslationUnit, TranslationUnit)
}

pub fn recognize_template_list(lexer: impl TokenIterator) -> Result<(), Error> {
    match parse_tokens(lexer, Token::EntryPointTryTemplateList) {
        Ok(ParseEntryPoint::TryTemplateList(_)) => Ok(()),
        Ok(_) => unreachable!("parser parsed the wrong entrypoint"),
        Err(e) => Err(e),
    }
}

impl FromStr for TranslationUnit {
    type Err = Error;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        parse!(source, EntryPointTranslationUnit, TranslationUnit)
    }
}
impl FromStr for GlobalDirective {
    type Err = Error;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        parse!(source, EntryPointGlobalDirective, GlobalDirective)
    }
}
impl FromStr for GlobalDeclaration {
    type Err = Error;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        parse!(source, EntryPointGlobalDecl, GlobalDecl)
    }
}
impl FromStr for Statement {
    type Err = Error;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        parse!(source, EntryPointStatement, Statement)
    }
}
impl FromStr for Expression {
    type Err = Error;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        parse!(source, EntryPointExpression, Expression)
    }
}
impl FromStr for LiteralExpression {
    type Err = Error;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        parse!(source, EntryPointLiteral, Literal)
    }
}
impl FromStr for crate::syntax::ImportStatement {
    type Err = Error;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        parse!(source, EntryPointImportStatement, ImportStatement)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn expect_err<T: FromStr + std::fmt::Display>(expr: &str)
    where
        <T as FromStr>::Err: std::fmt::Debug,
    {
        let parsed = T::from_str(expr);
        assert!(
            parsed.is_err(),
            "case `{expr}`: expected Err, got Ok({})",
            parsed.unwrap()
        );
    }

    fn expect_ok<T: FromStr + std::fmt::Debug>(expr: &str)
    where
        <T as FromStr>::Err: std::fmt::Display,
    {
        let parsed = T::from_str(expr);
        assert!(
            parsed.is_ok(),
            "case `{expr}`: expected Ok, got Err({})",
            parsed.unwrap_err()
        );
    }

    #[test]
    fn operator_prec_assoc() {
        // failure cases from the WGSL spec
        expect_ok::<Expression>("x & (y ^ (z | w))"); // Invalid: x & y ^ z | w
        expect_ok::<Expression>("(x + y) << (z >= w)"); // Invalid: x + y << z >= w
        expect_ok::<Expression>("x < (y > z)"); // Invalid: x < y > z
        expect_ok::<Expression>("x && (y || z)"); // Invalid: x && y || z
        expect_err::<Expression>("x & y ^ z | w");
        expect_err::<Expression>("x + y << z >= w");
        expect_err::<Expression>("x < y > z");
        expect_err::<Expression>("x && y || z");
        // more cases
        expect_ok::<Expression>("x && y && z");
        expect_ok::<Expression>("x || y || z");
        expect_ok::<Expression>("x & y & z");
        expect_ok::<Expression>("x ^ y ^ z");
        expect_ok::<Expression>("x | y | z");
        expect_err::<Expression>("x >> y >> z");
        expect_err::<Expression>("x << y << z");
        expect_ok::<Expression>("x && y < z && &w");
        expect_ok::<Expression>("x & -y & &z");
    }

    #[test]
    fn lhs_expression() {
        expect_ok::<Statement>("(x) = 1;");
        expect_ok::<Statement>("(*x) += 1;");
        expect_ok::<Statement>("&(*x) += 1;");
        expect_ok::<Statement>("x[0] = 1;");
        expect_ok::<Statement>("x.y[0] = 1;");
        expect_ok::<Statement>("x[0].y = 1;");

        // WESL extensions
        expect_ok::<Statement>("@if(true) x = 1;");
        expect_ok::<Statement>("@if(true) &(*x) += 1;");
        expect_err::<Statement>("@if(true) (*x) += 1;");
        expect_ok::<Statement>("x::y = 1;");
    }

    #[test]
    fn wildcard_imports() {
        expect_ok::<ImportStatement>("import foo::*;");
        expect_ok::<ImportStatement>("import foo::bar::*;");
        expect_ok::<ImportStatement>("import package::*;");
        expect_ok::<ImportStatement>("import super::*;");
        expect_ok::<ImportStatement>("import foo::{a::b, *};");
        expect_ok::<ImportStatement>("import foo::{a::*, b as c};");
        expect_ok::<ImportStatement>("import {foo::*};");
        expect_ok::<ImportStatement>("@if(true) import foo::*;");
        expect_ok::<ImportStatement>("@diagnostic(off, wildcard_shadow) import foo::*;");
        expect_err::<ImportStatement>("import *;");
        expect_err::<ImportStatement>("import {*};");
        expect_err::<ImportStatement>("import {a, *};");
        expect_err::<ImportStatement>("import foo::* as bar;");
        expect_err::<ImportStatement>("import foo::**;");
    }

    #[test]
    fn public_wildcard_imports_are_reserved() {
        expect_ok::<ImportStatement>("public import foo::bar;");
        expect_err::<ImportStatement>("public import foo::*;");
        expect_err::<ImportStatement>("public import foo::{bar, *};");
        expect_err::<ImportStatement>("public import foo::{bar::*};");
    }

    #[test]
    fn wildcard_import_structure() {
        let stmt = ImportStatement::from_str("import foo::{a::b, *};").unwrap();
        let ImportContent::Collection(coll) = stmt.content else {
            panic!("expected a collection");
        };
        assert_eq!(coll.len(), 2);
        assert!(coll[0].content.is_item());
        assert_eq!(coll[0].path, vec!["a".to_string()]);
        assert!(coll[1].content.is_wildcard());
        assert!(coll[1].path.is_empty());

        let stmt = ImportStatement::from_str("import foo::bar::*;").unwrap();
        assert!(stmt.content.is_wildcard());
        assert_eq!(stmt.path.unwrap().components, vec!["bar".to_string()]);
    }

    #[test]
    fn wildcard_imports_display() {
        for source in [
            "import foo::*;",
            "import foo::bar::*;",
            "import foo::{ a::b, * };",
        ] {
            let stmt = ImportStatement::from_str(source).unwrap();
            assert_eq!(stmt.to_string(), source);
        }
    }

    #[test]
    fn module_attributes() {
        expect_ok::<GlobalDirective>("@!wildcardable;");
        expect_ok::<GlobalDirective>("@!other(1, foo);");
        expect_ok::<GlobalDirective>("@if(true) @!wildcardable;");
        expect_err::<GlobalDirective>("@!wildcardable");
        expect_err::<GlobalDirective>("@! wildcardable extra;");
        expect_err::<GlobalDirective>("@!;");

        let unit = TranslationUnit::from_str(
            "import foo::*; @!wildcardable; enable f16; @!other(1); fn f() {}",
        )
        .unwrap();
        assert_eq!(unit.global_directives.len(), 3);
        assert!(unit.global_directives[0].is_module_attribute());
        assert!(unit.global_directives[1].is_enable());
        assert!(unit.global_directives[2].is_module_attribute());
    }

    #[test]
    fn module_attributes_display() {
        for source in ["@!wildcardable;", "@!other(1, foo);"] {
            let directive = GlobalDirective::from_str(source).unwrap();
            assert_eq!(directive.to_string(), source);
        }
    }

    #[test]
    fn attributes_are_not_module_attributes() {
        expect_ok::<Statement>("@if(true) x = 1;");
        expect_err::<Statement>("@!wildcardable x = 1;");
        expect_err::<GlobalDeclaration>("@!wildcardable fn f() {}");
    }
}

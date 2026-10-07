use crate::{
    span::Spanned,
    syntax::*,
    syntax_writer::{Print, SyntaxWriter},
};
use core::fmt;
use std::fmt::{Display, Formatter, Write as _};

/// Implements [`Display`] for syntax nodes by printing them with a [`SyntaxWriter`].
macro_rules! impl_display {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl Display for $ty {
                fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                    self.print(&mut SyntaxWriter::new(f))
                }
            }
        )+
    };
}

impl<T: Print> Display for Spanned<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.print(&mut SyntaxWriter::new(f))
    }
}

impl<T: Print> Print for Spanned<T> {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        w.with_span(self.span(), |w| self.node().print(w))
    }
}

/// Prints `attributes` separated by spaces, then a space if `inline` or a newline otherwise.
fn print_attributes(
    w: &mut SyntaxWriter<'_>,
    attributes: &[AttributeNode],
    inline: bool,
) -> fmt::Result {
    w.join(attributes, " ")?;
    match (attributes.is_empty(), inline) {
        (true, _) => Ok(()),
        (false, true) => w.write_str(" "),
        (false, false) => w.write_str("\n"),
    }
}

/// Prints `visibility` followed by a space, or nothing for package visibility.
fn print_visibility(w: &mut SyntaxWriter<'_>, visibility: Visibility) -> fmt::Result {
    match visibility {
        Visibility::Public => w.write_str("public "),
        Visibility::Package => Ok(()),
        Visibility::Private => w.write_str("private "),
    }
}

/// Prints an attribute that takes one expression, like `@group(0)`.
fn print_attribute_call(
    w: &mut SyntaxWriter<'_>,
    name: &str,
    argument: &ExpressionNode,
) -> fmt::Result {
    write!(w, "@{name}(")?;
    w.print(argument)?;
    w.write_str(")")
}

impl Print for TranslationUnit {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        for import in &self.imports {
            w.print(import)?;
            w.write_str("\n\n")?;
        }
        if !self.global_directives.is_empty() {
            w.join(&self.global_directives, "\n")?;
            w.write_str("\n\n")?;
        }
        let declarations = self
            .global_declarations
            .iter()
            .filter(|decl| !matches!(decl.node(), GlobalDeclaration::Void));
        w.join(declarations, "\n\n")?;
        w.write_str("\n")
    }
}

impl Print for Ident {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        w.write_str(&self.name())
    }
}

impl Print for Visibility {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match self {
            Visibility::Private => w.write_str("private"),
            Visibility::Package => w.write_str("package"),
            Visibility::Public => w.write_str("public"),
        }
    }
}

impl Print for ImportStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        print_visibility(w, self.visibility)?;
        w.write_str("import ")?;
        if let Some(path) = &self.path {
            w.print(path)?;
            w.write_str("::")?;
        }
        w.print(&self.content)?;
        w.write_str(";")
    }
}

impl Print for ModulePath {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match &self.origin {
            PathOrigin::Absolute => w.write_str("package")?,
            PathOrigin::Relative(0) => w.write_str("self")?,
            PathOrigin::Relative(n) => w.join_by(0..*n, "::", |w, _| w.write_str("super"))?,
            PathOrigin::Package(p) => w.write_str(p)?,
        }
        if !self.components.is_empty() {
            w.write_str("::")?;
            w.join_by(&self.components, "::", |w, c| w.write_str(c))?;
        }
        Ok(())
    }
}

impl Print for Import {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        if !self.path.is_empty() {
            w.join_by(&self.path, "::", |w, c| w.write_str(c))?;
            w.write_str("::")?;
        }
        w.print(&self.content)
    }
}

impl Print for ImportContent {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match self {
            ImportContent::Item(item) => {
                w.print(&item.ident)?;
                if let Some(rename) = &item.rename {
                    w.write_str(" as ")?;
                    w.print(rename)?;
                }
                Ok(())
            }
            ImportContent::Collection(coll) => {
                w.write_str("{ ")?;
                w.join(coll, ", ")?;
                w.write_str(" }")
            }
        }
    }
}

impl Print for GlobalDirective {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match self {
            GlobalDirective::Diagnostic(print) => w.print(print),
            GlobalDirective::Enable(print) => w.print(print),
            GlobalDirective::Requires(print) => w.print(print),
        }
    }
}

impl Print for DiagnosticDirective {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        write!(w, "diagnostic ({}, {});", self.severity, self.rule_name)
    }
}

impl Print for EnableDirective {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("enable ")?;
        w.join_by(&self.extensions, ", ", |w, ext| write!(w, "{ext}"))?;
        w.write_str(";")
    }
}

impl Print for RequiresDirective {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("requires ")?;
        w.join_by(&self.extensions, ", ", |w, ext| write!(w, "{ext}"))?;
        w.write_str(";")
    }
}

impl Print for GlobalDeclaration {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match self {
            GlobalDeclaration::Void => w.write_str(";"),
            GlobalDeclaration::Declaration(print) => w.print(print),
            GlobalDeclaration::TypeAlias(print) => w.print(print),
            GlobalDeclaration::Struct(print) => w.print(print),
            GlobalDeclaration::Function(print) => w.print(print),
            GlobalDeclaration::ConstAssert(print) => w.print(print),
            GlobalDeclaration::Compound(print) => w.print(print),
        }
    }
}

impl Print for Declaration {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        print_visibility(w, self.visibility)?;
        w.print(&self.kind)?;
        w.write_str(" ")?;
        w.print(&self.ident)?;
        if let Some(ty) = &self.ty {
            w.write_str(": ")?;
            w.print(ty)?;
        }
        if let Some(init) = &self.initializer {
            w.write_str(" = ")?;
            w.print(init)?;
        }
        w.write_str(";")
    }
}

impl Print for DeclarationKind {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match self {
            Self::Const => w.write_str("const"),
            Self::Override => w.write_str("override"),
            Self::Let => w.write_str("let"),
            Self::Var(None) => w.write_str("var"),
            Self::Var(Some((a_s, None))) => write!(w, "var<{a_s}>"),
            Self::Var(Some((a_s, Some(a_m)))) => write!(w, "var<{a_s}, {a_m}>"),
        }
    }
}

impl Print for TypeAlias {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        print_visibility(w, self.visibility)?;
        w.write_str("alias ")?;
        w.print(&self.ident)?;
        w.write_str(" = ")?;
        w.print(&self.ty)?;
        w.write_str(";")
    }
}

impl Print for Struct {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        print_visibility(w, self.visibility)?;
        w.write_str("struct ")?;
        w.print(&self.ident)?;
        w.write_str(" {\n")?;
        w.indented(|w| w.join(&self.members, ",\n"))?;
        w.write_str("\n}")
    }
}

impl Print for StructMember {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.print(&self.ident)?;
        w.write_str(": ")?;
        w.print(&self.ty)
    }
}

impl Print for Function {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        print_visibility(w, self.visibility)?;
        w.write_str("fn ")?;
        w.print(&self.ident)?;
        w.write_str("(")?;
        w.join(&self.parameters, ", ")?;
        w.write_str(") ")?;
        if let Some(ty) = &self.return_type {
            w.write_str("-> ")?;
            print_attributes(w, &self.return_attributes, true)?;
            w.print(ty)?;
            w.write_str(" ")?;
        }
        w.print(&self.body)
    }
}

impl Print for FormalParameter {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, true)?;
        w.print(&self.ident)?;
        w.write_str(": ")?;
        w.print(&self.ty)
    }
}

impl Print for ConstAssert {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("const_assert ")?;
        w.print(&self.expression)?;
        w.write_str(";")
    }
}

impl Print for CompoundGlobalDeclaration {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("{\n")?;
        w.indented(|w| w.join(&self.body, "\n"))?;
        w.write_str("\n}")
    }
}

impl Print for Attribute {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match self {
            Attribute::Align(e1) => print_attribute_call(w, "align", e1),
            Attribute::Binding(e1) => print_attribute_call(w, "binding", e1),
            Attribute::BlendSrc(e1) => print_attribute_call(w, "blend_src", e1),
            Attribute::Builtin(e1) => write!(w, "@builtin({e1})"),
            Attribute::Const => w.write_str("@const"),
            Attribute::Diagnostic(DiagnosticAttribute { severity, rule }) => {
                write!(w, "@diagnostic({severity}, {rule})")
            }
            Attribute::Group(e1) => print_attribute_call(w, "group", e1),
            Attribute::Id(e1) => print_attribute_call(w, "id", e1),
            Attribute::Interpolate(InterpolateAttribute { ty, sampling }) => {
                write!(w, "@interpolate({ty}")?;
                if let Some(sampling) = sampling {
                    write!(w, ", {sampling}")?;
                }
                w.write_str(")")
            }
            Attribute::Invariant => w.write_str("@invariant"),
            Attribute::Location(e1) => print_attribute_call(w, "location", e1),
            Attribute::MustUse => w.write_str("@must_use"),
            Attribute::Size(e1) => print_attribute_call(w, "size", e1),
            Attribute::WorkgroupSize(WorkgroupSizeAttribute { x, y, z }) => {
                w.write_str("@workgroup_size(")?;
                w.join(std::iter::once(x).chain(y).chain(z), ", ")?;
                w.write_str(")")
            }
            Attribute::Vertex => w.write_str("@vertex"),
            Attribute::Fragment => w.write_str("@fragment"),
            Attribute::Compute => w.write_str("@compute"),

            // wesl extensions
            Attribute::If(e1) => print_attribute_call(w, "if", e1),
            Attribute::Elif(e1) => print_attribute_call(w, "elif", e1),
            Attribute::Else => w.write_str("@else"),
            #[cfg(feature = "generics")]
            Attribute::Type(e1) => {
                w.write_str("@type(")?;
                w.print(e1)?;
                w.write_str(")")
            }

            // naga extensions
            #[cfg(feature = "naga-ext")]
            Attribute::Task => w.write_str("@task"),
            #[cfg(feature = "naga-ext")]
            Attribute::Payload(p) => print_attribute_call(w, "payload", p),
            #[cfg(feature = "naga-ext")]
            Attribute::Mesh(m) => print_attribute_call(w, "mesh", m),
            #[cfg(feature = "naga-ext")]
            Attribute::RayGeneration => w.write_str("@ray_generation"),
            #[cfg(feature = "naga-ext")]
            Attribute::AnyHit => w.write_str("@any_hit"),
            #[cfg(feature = "naga-ext")]
            Attribute::ClosestHit => w.write_str("@closest_hit"),
            #[cfg(feature = "naga-ext")]
            Attribute::Miss => w.write_str("@miss"),
            #[cfg(feature = "naga-ext")]
            Attribute::IncomingPayload(p) => print_attribute_call(w, "incoming_payload", p),
            #[cfg(feature = "naga-ext")]
            Attribute::EarlyDepthTest(None) => w.write_str("@early_depth_test"),
            #[cfg(feature = "naga-ext")]
            Attribute::EarlyDepthTest(Some(e1)) => write!(w, "@early_depth_test({e1})"),
            Attribute::Custom(custom) => {
                write!(w, "@{}", custom.name)?;
                if let Some(args) = &custom.arguments {
                    w.write_str("(")?;
                    w.join(args, ", ")?;
                    w.write_str(")")?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(feature = "generics")]
impl Print for TypeConstraint {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        w.print(&self.ident)?;
        w.write_str(", ")?;
        w.join(&self.variants, " | ")
    }
}

impl Print for Expression {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match self {
            Expression::Literal(print) => w.print(print),
            Expression::Parenthesized(print) => w.print(print),
            Expression::NamedComponent(print) => w.print(print),
            Expression::Indexing(print) => w.print(print),
            Expression::Unary(print) => w.print(print),
            Expression::Binary(print) => w.print(print),
            Expression::FunctionCall(print) => w.print(print),
            Expression::TypeOrIdentifier(print) => w.print(print),
        }
    }
}

impl Print for LiteralExpression {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match self {
            LiteralExpression::Bool(true) => w.write_str("true"),
            LiteralExpression::Bool(false) => w.write_str("false"),
            LiteralExpression::AbstractInt(num) => write!(w, "{num}"),
            // the debug format keeps the trailing `.0` of floats that represent integers
            LiteralExpression::AbstractFloat(num) => write!(w, "{num:?}"),
            LiteralExpression::I32(num) => write!(w, "{num}i"),
            LiteralExpression::U32(num) => write!(w, "{num}u"),
            LiteralExpression::F32(num) => write!(w, "{num}f"),
            LiteralExpression::F16(num) => write!(w, "{num}h"),
            #[cfg(feature = "naga-ext")]
            LiteralExpression::I64(num) => write!(w, "{num}li"),
            #[cfg(feature = "naga-ext")]
            LiteralExpression::U64(num) => write!(w, "{num}lu"),
            #[cfg(feature = "naga-ext")]
            LiteralExpression::F64(num) => write!(w, "{num}lf"),
        }
    }
}

impl Print for ParenthesizedExpression {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        w.write_str("(")?;
        w.print(&self.expression)?;
        w.write_str(")")
    }
}

impl Print for NamedComponentExpression {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        w.print(&self.base)?;
        w.write_str(".")?;
        w.print(&self.component)
    }
}

impl Print for IndexingExpression {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        w.print(&self.base)?;
        w.write_str("[")?;
        w.print(&self.index)?;
        w.write_str("]")
    }
}

impl Print for UnaryExpression {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        write!(w, "{}", self.operator)?;
        w.print(&self.operand)
    }
}

impl Print for BinaryExpression {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        w.print(&self.left)?;
        write!(w, " {} ", self.operator)?;
        w.print(&self.right)
    }
}

impl Print for FunctionCall {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        w.print(&self.ty)?;
        w.write_str("(")?;
        w.join(&self.arguments, ", ")?;
        w.write_str(")")
    }
}

impl Print for TypeExpression {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        if let Some(path) = &self.path {
            w.print(path)?;
            w.write_str("::")?;
        }
        w.print(&self.ident)?;
        if let Some(template_args) = &self.template_args {
            w.write_str("<")?;
            w.join(template_args, ", ")?;
            w.write_str(">")?;
        }
        Ok(())
    }
}

impl Print for TemplateArg {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        w.print(&self.expression)
    }
}

impl Print for Statement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match self {
            Statement::Void => w.write_str(";"),
            Statement::Compound(print) => w.print(print),
            Statement::Assignment(print) => w.print(print),
            Statement::Increment(print) => w.print(print),
            Statement::Decrement(print) => w.print(print),
            Statement::If(print) => w.print(print),
            Statement::Switch(print) => w.print(print),
            Statement::Loop(print) => w.print(print),
            Statement::For(print) => w.print(print),
            Statement::While(print) => w.print(print),
            Statement::Break(print) => w.print(print),
            Statement::Continue(print) => w.print(print),
            Statement::Return(print) => w.print(print),
            Statement::Discard(print) => w.print(print),
            Statement::FunctionCall(print) => w.print(print),
            Statement::ConstAssert(print) => w.print(print),
            Statement::Declaration(print) => w.print(print),
        }
    }
}

/// Prints the statements of a block, one per line, skipping empty statements.
fn print_statements(w: &mut SyntaxWriter<'_>, statements: &[StatementNode]) -> fmt::Result {
    let statements = statements
        .iter()
        .filter(|stmt| !matches!(stmt.node(), Statement::Void));
    w.join(statements, "\n")
}

impl Print for CompoundStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("{\n")?;
        w.indented(|w| print_statements(w, &self.statements))?;
        w.write_str("\n}")
    }
}

impl Print for AssignmentStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.print(&self.lhs)?;
        write!(w, " {} ", self.operator)?;
        w.print(&self.rhs)?;
        w.write_str(";")
    }
}

impl Print for IncrementStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.print(&self.expression)?;
        w.write_str("++;")
    }
}

impl Print for DecrementStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.print(&self.expression)?;
        w.write_str("--;")
    }
}

impl Print for IfStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.print(&self.if_clause)?;
        for else_if_clause in &self.else_if_clauses {
            w.write_str("\n")?;
            w.print(else_if_clause)?;
        }
        if let Some(else_clause) = &self.else_clause {
            w.write_str("\n")?;
            w.print(else_clause)?;
        }
        Ok(())
    }
}

impl Print for IfClause {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        w.write_str("if ")?;
        w.print(&self.expression)?;
        w.write_str(" ")?;
        w.print(&self.body)
    }
}

impl Print for ElseIfClause {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("else if ")?;
        w.print(&self.expression)?;
        w.write_str(" ")?;
        w.print(&self.body)
    }
}

impl Print for ElseClause {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("else ")?;
        w.print(&self.body)
    }
}

impl Print for SwitchStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("switch ")?;
        w.print(&self.expression)?;
        w.write_str(" ")?;
        print_attributes(w, &self.body_attributes, false)?;
        w.write_str("{\n")?;
        w.indented(|w| w.join(&self.clauses, "\n"))?;
        w.write_str("\n}")
    }
}

impl Print for SwitchClause {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("case ")?;
        w.join(&self.case_selectors, ", ")?;
        w.write_str(" ")?;
        w.print(&self.body)
    }
}

impl Print for CaseSelector {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        match self {
            CaseSelector::Default => w.write_str("default"),
            CaseSelector::Expression(expr) => w.print(expr),
        }
    }
}

impl Print for LoopStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("loop ")?;
        print_attributes(w, &self.body.attributes, false)?;
        w.write_str("{\n")?;
        w.indented(|w| print_statements(w, &self.body.statements))?;
        w.write_str("\n")?;
        if let Some(continuing) = &self.continuing {
            w.indented(|w| w.print(continuing))?;
            w.write_str("\n")?;
        }
        w.write_str("}")
    }
}

impl Print for ContinuingStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("continuing ")?;
        print_attributes(w, &self.body.attributes, false)?;
        w.write_str("{\n")?;
        w.indented(|w| print_statements(w, &self.body.statements))?;
        w.write_str("\n")?;
        if let Some(break_if) = &self.break_if {
            w.indented(|w| w.print(break_if))?;
            w.write_str("\n")?;
        }
        w.write_str("}")
    }
}

impl Print for BreakIfStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("break if ")?;
        w.print(&self.expression)?;
        w.write_str(";")
    }
}

impl Print for ForStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("for (")?;
        if let Some(initializer) = &self.initializer {
            w.without_trailing_semicolon(|w| w.print(initializer))?;
        }
        w.write_str("; ")?;
        if let Some(condition) = &self.condition {
            w.print(condition)?;
        }
        w.write_str("; ")?;
        if let Some(update) = &self.update {
            w.without_trailing_semicolon(|w| w.print(update))?;
        }
        w.write_str(") ")?;
        w.print(&self.body)
    }
}

impl Print for WhileStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("while ")?;
        w.print(&self.condition)?;
        w.write_str(" ")?;
        w.print(&self.body)
    }
}

impl Print for BreakStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("break;")
    }
}

impl Print for ContinueStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("continue;")
    }
}

impl Print for ReturnStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("return")?;
        if let Some(expression) = &self.expression {
            w.write_str(" ")?;
            w.print(expression)?;
        }
        w.write_str(";")
    }
}

impl Print for DiscardStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.write_str("discard;")
    }
}

impl Print for FunctionCallStatement {
    fn print(&self, w: &mut SyntaxWriter<'_>) -> fmt::Result {
        print_attributes(w, &self.attributes, false)?;
        w.print(&self.call)?;
        w.write_str(";")
    }
}

impl_display!(
    TranslationUnit,
    Ident,
    Visibility,
    ImportStatement,
    ModulePath,
    Import,
    ImportContent,
    GlobalDirective,
    DiagnosticDirective,
    EnableDirective,
    RequiresDirective,
    GlobalDeclaration,
    Declaration,
    DeclarationKind,
    TypeAlias,
    Struct,
    StructMember,
    Function,
    FormalParameter,
    ConstAssert,
    CompoundGlobalDeclaration,
    Attribute,
    Expression,
    LiteralExpression,
    ParenthesizedExpression,
    NamedComponentExpression,
    IndexingExpression,
    UnaryExpression,
    BinaryExpression,
    FunctionCall,
    TypeExpression,
    TemplateArg,
    Statement,
    CompoundStatement,
    AssignmentStatement,
    IncrementStatement,
    DecrementStatement,
    IfStatement,
    IfClause,
    ElseIfClause,
    ElseClause,
    SwitchStatement,
    SwitchClause,
    CaseSelector,
    LoopStatement,
    ContinuingStatement,
    BreakIfStatement,
    ForStatement,
    WhileStatement,
    BreakStatement,
    ContinueStatement,
    ReturnStatement,
    DiscardStatement,
    FunctionCallStatement,
);

#[cfg(feature = "generics")]
impl_display!(TypeConstraint);

#[cfg(test)]
mod test {
    use crate::syntax::ModulePath;
    use crate::syntax::{Ident, TypeExpression};

    #[test]
    fn type_expression_display() {
        let expr = TypeExpression {
            path: None,
            ident: Ident::new("foo".into()),
            template_args: None,
        };

        assert_eq!(expr.to_string(), "foo");

        let expr = TypeExpression {
            path: Some(ModulePath::new(
                crate::syntax::PathOrigin::Absolute,
                vec!["bar".into(), "qux".into()],
            )),
            ident: Ident::new("foo".into()),
            template_args: None,
        };

        assert_eq!(expr.to_string(), "package::bar::qux::foo");
    }
}

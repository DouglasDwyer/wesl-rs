use std::collections::HashMap;

use crate::{error::Error, pass::Visit};

use wgsl_parse::{SyntaxNode, syntax::*};

/// Performs conversions on the final syntax tree to make it more compatible with WGSL
/// implementations like Naga, catch errors early and perform optimizations.
///
/// Currently, `lower` performs the following transforms:
/// * remove aliases (inlined)
/// * remove consts (inlined)
/// * remove deprecated, non-standard attributes
/// * remove import declarations
///
/// with the `eval` feature flag enabled, it performs additional transforms:
/// * evaluate const-expressions (including calls to const functions)
/// * remove unreachable code paths after const-evaluation
/// * remove function call statements to const functions (no side-effects)
/// * make implicit conversions from abstract types explicit (using conversion rank)
///
/// Customizing this behavior is not possible currently. The following transforms may
/// be available in the future:
/// * make variable types explicit
/// * remove unused variables / code with no side-effects
pub fn lower(module: &mut TranslationUnit) -> Result<(), Error> {
    module.imports.clear();

    for attrs in Visit::<Attributes>::visit_mut(module) {
        attrs.retain(|attr| {
            !matches!(attr.node(),
            Attribute::Custom(CustomAttribute { name, .. }) if name == "generic")
        })
    }

    #[cfg(not(feature = "eval"))]
    {
        // these are redundant with eval::lower.
        inline_type_aliases(module);
        inline_global_consts(module);
    }
    #[cfg(feature = "eval")]
    {
        use crate::error::Diagnostic;
        use crate::eval::{Context, Exec, Lower, mark_functions_const};
        use wgsl_parse::SyntaxNode;
        mark_functions_const(module);

        // we want to drop wesl2 at the end of the block for idents use_count
        {
            let module2 = module.clone();
            let mut ctx = Context::new(&module2);
            module
                .exec(&mut ctx) // populate the ctx with module-scope declarations
                .map_err(|e| Diagnostic::from(e).with_ctx(&ctx))?;
            module
                .lower(&mut ctx)
                .map_err(|e| Diagnostic::from(e).with_ctx(&ctx))?;
        }

        // remove `@const` attributes.
        for decl in &mut module.global_declarations {
            if let GlobalDeclaration::Function(decl) = decl.node_mut() {
                decl.retain_attributes_mut(|attr| *attr != Attribute::Const);
            }
        }
    }
    Ok(())
}

/// Eliminate all type aliases.
#[allow(unused)]
fn inline_type_aliases(wesl: &mut TranslationUnit) {
    let aliases = take_declarations_in_dependency_order(wesl, |decl| {
        matches!(decl, GlobalDeclaration::TypeAlias(_))
    });

    for alias in aliases {
        let GlobalDeclaration::TypeAlias(mut alias) = alias else {
            unreachable!()
        };
        // we rename the alias and all references to its type expression,
        // and drop the alias declaration.
        alias.ident.rename(format!("{}", alias.ty));
    }
}

/// Eliminate all const-declarations.
///
/// Replace usages of the const-declaration with its expression.
///
/// # Panics
///
/// panics if the const-declaration is ill-formed, i.e. has no initializer.
#[allow(unused)]
fn inline_global_consts(wesl: &mut TranslationUnit) {
    let consts = take_declarations_in_dependency_order(wesl, |decl| {
        matches!(
            decl,
            GlobalDeclaration::Declaration(Declaration {
                kind: DeclarationKind::Const,
                ..
            })
        )
    });

    for decl in consts {
        let GlobalDeclaration::Declaration(mut decl) = decl else {
            unreachable!()
        };
        // we rename the const and all references to its expression in parentheses,
        // and drop the const declaration.
        decl.ident
            .rename(format!("({})", decl.initializer.unwrap()));
    }
}

/// Removes the global declarations matching `filter` from `wesl`, and returns them
/// ordered such that every declaration comes after the matching declarations it refers to.
///
/// Inlining renames a declaration's ident to the text of its definition. That text is
/// computed from the identifiers *at that moment*, so a declaration must only be inlined
/// after all the declarations it references have been. Otherwise, the text would contain the
/// stale name of an item that is about to be removed. The order of declarations in `wesl` is
/// not enough to guarantee this, because it depends on the order modules were linked.
///
/// Ties are broken by the original order of the declarations. If the declarations are
/// cyclic (which is invalid), the cycle is broken arbitrarily.
fn take_declarations_in_dependency_order(
    wesl: &mut TranslationUnit,
    filter: impl Fn(&GlobalDeclaration) -> bool,
) -> Vec<GlobalDeclaration> {
    let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut wesl.global_declarations)
        .into_iter()
        .partition(|decl| filter(decl.node()));
    wesl.global_declarations = kept;

    let taken = taken
        .into_iter()
        .map(|decl| decl.into_inner())
        .collect::<Vec<_>>();

    let index_of = taken
        .iter()
        .enumerate()
        .filter_map(|(index, decl)| Some((decl.ident()?, index)))
        .collect::<HashMap<Ident, usize>>();

    // for each declaration, the indices of the other declarations it refers to
    let dependencies = taken
        .iter()
        .enumerate()
        .map(|(index, decl)| {
            let mut deps = Vec::new();
            Visit::<TypeExpression>::visit_rec(decl, &mut |ty_expr| {
                if let Some(&dep) = index_of.get(&ty_expr.ident)
                    && dep != index
                {
                    deps.push(dep);
                }
            });
            deps
        })
        .collect::<Vec<_>>();

    // depth-first post-order traversal
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unvisited,
        InProgress,
        Done,
    }

    fn visit(
        index: usize,
        dependencies: &[Vec<usize>],
        states: &mut [State],
        order: &mut Vec<usize>,
    ) {
        if states[index] != State::Unvisited {
            return;
        }
        states[index] = State::InProgress;
        for &dep in &dependencies[index] {
            visit(dep, dependencies, states, order);
        }
        states[index] = State::Done;
        order.push(index);
    }

    let mut states = vec![State::Unvisited; taken.len()];
    let mut order = Vec::with_capacity(taken.len());
    for index in 0..taken.len() {
        visit(index, &dependencies, &mut states, &mut order);
    }

    let mut taken = taken.into_iter().map(Some).collect::<Vec<_>>();
    order
        .into_iter()
        .map(|index| taken[index].take().expect("declaration visited twice"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lowered(source: &str) -> String {
        let mut module: TranslationUnit = source.parse().unwrap();
        // links references to their declarations, like the compiler does after loading a module
        crate::pass::retarget_idents(&mut module);
        lower(&mut module).unwrap();
        module.to_string()
    }

    #[test]
    fn inlined_const_referencing_later_const() {
        // `lower` must not depend on the order of declarations
        let wgsl = lowered(
            "const DERIVED: u32 = BASE << 1; const BASE: u32 = 3; fn f() -> u32 { return DERIVED; }",
        );
        assert!(
            !wgsl.contains("BASE"),
            "stale reference to inlined const: {wgsl}"
        );
        assert!(!wgsl.contains("DERIVED"), "const was not inlined: {wgsl}");
    }

    #[test]
    fn inlined_const_chain_in_any_order() {
        let wgsl = lowered(
            "const C: u32 = B + 1; const B: u32 = A + 1; const A: u32 = 1; fn f() -> u32 { return C; }",
        );
        for name in ["A", "B", "C"] {
            assert!(
                !wgsl.contains(&format!("{name} ")) && !wgsl.contains(&format!("({name}")),
                "stale reference to const `{name}`: {wgsl}"
            );
        }
    }

    // this tests `inline_type_aliases`, which is only used without the `eval` feature
    #[cfg(not(feature = "eval"))]
    #[test]
    fn inlined_alias_referencing_later_alias() {
        let wgsl = lowered("alias Derived = Base; alias Base = vec3f; var<private> x: Derived;");
        assert!(
            !wgsl.contains("Base"),
            "stale reference to inlined alias: {wgsl}"
        );
        assert!(!wgsl.contains("Derived"), "alias was not inlined: {wgsl}");
        assert!(wgsl.contains("vec3f"), "{wgsl}");
    }
}

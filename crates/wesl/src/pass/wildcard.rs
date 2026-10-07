use std::collections::{HashMap, HashSet};

use itertools::Itertools;
use wgsl_parse::{SyntaxNode, syntax::*};

use crate::{
    error::{UsageError, Warning},
    idents::builtin_ident,
    pass::{
        ImportedItem, Module, retarget_idents_with, suppressed_rules,
        usage_analysis::required_visibility,
    },
};

/// Name of the module attribute that allows wildcard imports from external packages.
const WILDCARDABLE: &str = "wildcardable";

/// Diagnostic rules that only the WESL compiler knows about.
const WESL_DIAGNOSTIC_RULES: [&str; 4] = [
    "unsupported_wildcard",
    "wildcard_shadow",
    "cross_package_wildcard",
    "builtin_shadow",
];

/// Modules that may be wildcard imported: the ones in use plus the ones loaded only to read
/// their items.
pub struct LoadedModules<'a> {
    /// Modules in use by the compilation.
    used: &'a [Module],
    /// Modules loaded to read their items, not (yet) in use.
    peeked: &'a HashMap<ModulePath, Module>,
}

impl<'a> LoadedModules<'a> {
    /// Create a view over the used and the peeked modules.
    pub fn new(used: &'a [Module], peeked: &'a HashMap<ModulePath, Module>) -> Self {
        Self { used, peeked }
    }

    /// Get a loaded module by its canonical path.
    pub fn get(&self, path: &ModulePath) -> Option<&'a Module> {
        self.used
            .iter()
            .find(|module| module.path == *path)
            .or_else(|| self.peeked.get(path))
    }
}

/// Whether the module is marked `@!wildcardable`.
pub fn is_wildcardable(module: &TranslationUnit) -> bool {
    module.global_directives.iter().any(|directive| {
        matches!(directive, GlobalDirective::ModuleAttribute(attr) if attr.name == WILDCARDABLE)
    })
}

/// Whether the module has at least one wildcard import.
pub fn has_wildcards(module: &TranslationUnit) -> bool {
    fn rec(content: &ImportContent) -> bool {
        match content {
            ImportContent::Item(_) => false,
            ImportContent::Wildcard => true,
            ImportContent::Collection(coll) => coll.iter().any(|import| rec(&import.content)),
        }
    }
    module.imports.iter().any(|import| rec(&import.content))
}

/// Diagnostic rules turned off for the whole module with `diagnostic(off, rule);`.
fn module_suppressed(module: &TranslationUnit) -> HashSet<&str> {
    module
        .global_directives
        .iter()
        .filter_map(|directive| match directive {
            GlobalDirective::Diagnostic(diag) if diag.severity == DiagnosticSeverity::Off => {
                Some(diag.rule_name.as_str())
            }
            _ => None,
        })
        .collect()
}

/// Names that a wildcard import of `module` provides, with their visibility.
///
/// These are the declarations of the module and its `public import` re-exports.
fn exports(module: &Module) -> impl Iterator<Item = (String, Visibility)> {
    let declarations = module.syntax.global_declarations.iter().filter_map(|decl| {
        decl.ident()
            .map(|ident| (ident.to_string(), decl.visibility()))
    });
    let reexports = module
        .imports
        .iter()
        .filter(|(_, item)| item.visibility == Visibility::Public)
        .map(|(ident, item)| (ident.to_string(), item.visibility));
    declarations.chain(reexports)
}

/// Bind the references that wildcard imports provide.
///
/// Gathers, for every wildcard import of `module`, the items of the imported module that
/// are visible to `module`. References to a name that `module` neither declares nor imports by
/// name are then bound to these items, taking precedence over WGSL built-ins. When several
/// wildcards provide the same name, the reference is checked by [`crate::pass::retarget_modules`].
///
/// All the modules that `module` wildcard imports must be available in `loaded`.
///
/// Returns the `wildcard_shadow` warnings, or the `unsupported_wildcard` and
/// `cross_package_wildcard` errors.
pub fn resolve_wildcards(
    module: &mut Module,
    loaded: &LoadedModules,
    canonical_path: impl Fn(&ModulePath) -> ModulePath,
    ignore_visibility: bool,
) -> Result<Vec<Warning>, UsageError> {
    if module.wildcards.is_empty() {
        return Ok(Vec::new());
    }

    let module_off = module_suppressed(&module.syntax);
    let mut candidates: HashMap<String, Vec<ImportedItem>> = HashMap::new();

    for wildcard in &module.wildcards {
        let path = canonical_path(&wildcard.path);
        if path == module.path {
            continue;
        }
        let Some(imported) = loaded.get(&path) else {
            debug_assert!(false, "wildcard import of unloaded module {path}");
            continue;
        };

        let off = |rule: &str| {
            module_off.contains(rule) || wildcard.suppressed.iter().any(|off| off == rule)
        };
        if path.origin != module.path.origin {
            if !is_wildcardable(&imported.syntax) && !off("unsupported_wildcard") {
                return Err(UsageError::UnsupportedWildcard(path));
            }
            if module.path.origin.is_package() && !off("cross_package_wildcard") {
                return Err(UsageError::CrossPackageWildcard(path));
            }
        }

        let min_vis = required_visibility(&module.path, &path);
        for (name, visibility) in exports(imported) {
            if !ignore_visibility && visibility < min_vis {
                continue;
            }
            let items = candidates.entry(name.clone()).or_default();
            if !items.iter().any(|item| item.path == path) {
                items.push(ImportedItem {
                    path: path.clone(),
                    ident: Ident::new(name),
                    visibility,
                });
            }
        }
    }

    let mut warnings = Vec::new();
    let mut bound_by_module = HashSet::new();

    for decl in &module.syntax.global_declarations {
        let Some(ident) = decl.ident() else { continue };
        let name = ident.to_string();
        if candidates.contains_key(&name) {
            let off = module_off.contains("wildcard_shadow")
                || suppressed_rules(decl.attributes()).any(|rule| rule == "wildcard_shadow");
            if !off {
                warnings.push(Warning::WildcardShadow {
                    module: module.path.clone(),
                    name: name.clone(),
                });
            }
        }
        bound_by_module.insert(name);
    }

    for statement in &module.syntax.imports {
        let (named, _) =
            crate::pass::import::flatten_all(std::slice::from_ref(statement), &module.path);
        let off = module_off.contains("wildcard_shadow")
            || suppressed_rules(&statement.attributes).any(|rule| rule == "wildcard_shadow");
        for (ident, item) in named {
            let name = ident.to_string();
            if let Some(items) = candidates.get(&name) {
                let same_item = items
                    .iter()
                    .all(|c| c.path == item.path && *c.ident.name() == *item.ident.name());
                if !off && !same_item {
                    warnings.push(Warning::WildcardShadow {
                        module: module.path.clone(),
                        name: name.clone(),
                    });
                }
            }
            bound_by_module.insert(name);
        }
    }

    let mut outer = HashMap::new();
    for (name, items) in candidates {
        if bound_by_module.contains(&name) {
            continue;
        }
        let ident = Ident::new(name.clone());
        module.wildcard_bindings.insert(ident.clone(), items);
        outer.insert(name, ident);
    }
    retarget_idents_with(&mut module.syntax, &outer);

    Ok(warnings.into_iter().unique().collect())
}

/// Warn about declarations of a `@!wildcardable` module that shadow WGSL built-ins.
pub fn builtin_shadow_warnings(module: &Module) -> Vec<Warning> {
    if !is_wildcardable(&module.syntax) {
        return Vec::new();
    }

    let module_off = module_suppressed(&module.syntax);
    if module_off.contains("builtin_shadow") {
        return Vec::new();
    }

    module
        .syntax
        .global_declarations
        .iter()
        .filter(|decl| !suppressed_rules(decl.attributes()).any(|rule| rule == "builtin_shadow"))
        .filter_map(|decl| decl.ident())
        .filter(|ident| builtin_ident(&ident.name()).is_some())
        .map(|ident| Warning::BuiltinShadow {
            module: module.path.clone(),
            name: ident.to_string(),
        })
        .collect()
}

/// Remove the diagnostic rules specific to WESL from the directives and the declaration
/// attributes of a linked module, since WGSL does not define them.
pub fn strip_wesl_diagnostics(module: &mut TranslationUnit) {
    module.global_directives.retain(|directive| {
        !matches!(
            directive,
            GlobalDirective::Diagnostic(diag)
                if WESL_DIAGNOSTIC_RULES.contains(&diag.rule_name.as_str())
        )
    });
    for decl in &mut module.global_declarations {
        decl.retain_attributes_mut(|attr| {
            !matches!(
                attr,
                Attribute::Diagnostic(diag)
                    if WESL_DIAGNOSTIC_RULES.contains(&diag.rule.as_str())
            )
        });
    }
}

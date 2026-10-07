use std::collections::{HashMap, hash_map::Entry};
use wgsl_parse::{SyntaxNode, syntax::*};

use crate::{
    error::UsageError,
    pass::{ImportedItem, Imports, Visit, WildcardImport, import::flatten_all, imported_item_path},
};

#[derive(Clone)]
pub struct Module {
    pub syntax: TranslationUnit,
    pub path: ModulePath,
    pub imports: Imports,
    /// Wildcard imports (`import path::*;`) of the module.
    pub wildcards: Vec<WildcardImport>,
    /// Items that wildcard imports may provide, keyed by the ident that references to them
    /// are bound to. Filled by [`crate::pass::resolve_wildcards`].
    pub wildcard_bindings: HashMap<Ident, Vec<ImportedItem>>,
}

impl Module {
    pub fn new(path: ModulePath, syntax: TranslationUnit) -> Self {
        let (imports, wildcards) = flatten_all(&syntax.imports, &path);
        Self {
            syntax,
            path,
            imports,
            wildcards,
            wildcard_bindings: HashMap::new(),
        }
    }

    /// Items an unqualified reference may resolve to through wildcard imports.
    pub fn wildcard_items(&self, ty_expr: &TypeExpression) -> &[ImportedItem] {
        if ty_expr.path.is_some() {
            return &[];
        }
        self.wildcard_bindings
            .get(&ty_expr.ident)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// Minimum visibility an item in `item_path` needs to be used from the module at `module_path`.
pub(crate) fn required_visibility(module_path: &ModulePath, item_path: &ModulePath) -> Visibility {
    if module_path.origin == item_path.origin {
        Visibility::Package
    } else {
        Visibility::Public
    }
}

#[derive(Default, Clone, Debug)]
pub struct UsedItems {
    /// Module declarations used.
    used_items: HashMap<ModulePath, HashMap<Ident, Visibility>>,
}

/// Just a convenience
impl std::fmt::Display for UsedItems {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use itertools::Itertools;
        for (path, items) in self.iter() {
            writeln!(
                f,
                "{path} -> {}",
                items.keys().map(|ident| ident.to_string()).format(", ")
            )?;
        }
        Ok(())
    }
}

impl UsedItems {
    pub fn new() -> Self {
        Self {
            used_items: Default::default(),
        }
    }

    pub fn get_module(&self, path: &ModulePath) -> Option<&HashMap<Ident, Visibility>> {
        self.used_items.get(path)
    }

    pub fn contains_module(&self, path: &ModulePath) -> bool {
        self.used_items.contains_key(path)
    }

    pub fn get_ident(&self, path: &ModulePath, ident: &Ident) -> Option<Visibility> {
        self.used_items
            .get(path)
            .and_then(|items| items.get(ident).copied())
    }

    pub fn get_name(&self, path: &ModulePath, name: &str) -> Option<(Ident, Visibility)> {
        self.used_items.get(path).and_then(|items| {
            items
                .iter()
                .find(|(ident, _vis)| &**ident.name() == name)
                .map(|(ident, vis)| (ident.clone(), *vis))
        })
    }

    /// Returns true if inserted.
    pub fn insert_module(&mut self, path: ModulePath, idents: HashMap<Ident, Visibility>) -> bool {
        match self.used_items.entry(path) {
            Entry::Occupied(_) => false,
            Entry::Vacant(entry) => {
                entry.insert(idents);
                true
            }
        }
    }

    /// Returns true if inserted.
    /// Sets the visibility to max(vis, old_vis) if there was an entry.
    pub fn insert_ident(&mut self, path: ModulePath, ident: Ident, visibility: Visibility) -> bool {
        let entry = self.used_items.entry(path.clone()).or_default();

        match entry.entry(ident) {
            Entry::Occupied(mut entry) => {
                if *entry.get() < visibility {
                    entry.insert(visibility);
                }
                false
            }
            Entry::Vacant(entry) => {
                entry.insert(visibility);
                true
            }
        }
    }

    /// Returns true if deleted.
    pub fn remove_module(&mut self, path: &ModulePath) -> bool {
        self.used_items.remove(path).is_some()
    }

    pub fn is_empty(&mut self) -> bool {
        self.used_items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ModulePath, &HashMap<Ident, Visibility>)> {
        self.used_items.iter()
    }
}

/// Find declarations used in external modules which this module depends on, no matter what.
///
/// Currently, only items referenced by module-scope `const_assert`s are always included.
///
/// See [`usage_analysis`].
pub fn module_usage_analysis(
    module: &Module,
    already_used: &mut UsedItems,
    to_analyze: &mut UsedItems,
    ignore_visibility: bool,
) -> Result<(), UsageError> {
    if already_used.contains_module(&module.path) {
        return Ok(());
    }

    already_used.insert_module(module.path.clone(), Default::default());
    let const_asserts = module
        .syntax
        .global_declarations
        .iter()
        .filter(|decl| decl.is_const_assert());

    for decl in const_asserts {
        decl_usage_analysis(module, decl, already_used, to_analyze, ignore_visibility)?;
    }

    Ok(())
}

/// Find declaration names which a local declaration depends on.
///
/// Adds *external* referenced idents to `to_analyze` with their minimum required visibility.
/// Perform usage analysis recursively with *local* referenced idents, and adds them to `already_used`
/// with their declaration visibility.
/// So at the end of the call, `to_analyze` contains incomplete usage analysis which needs to continue
/// in a separate module. `already_used` contains finished analysis.
///
/// `min_vis` is the minimum visibility requirement for the declaration.
///
/// Returns [`UsageError::NotFound`] if the declaration is not found.
/// Returns [`UsageError::Visibility`] if the declaration is found, but does not have the
/// required visibility.
pub fn usage_analysis(
    module: &Module,
    decl_name: &str,
    min_vis: Visibility,
    already_used: &mut UsedItems,
    to_analyze: &mut UsedItems,
    ignore_visibility: bool,
) -> Result<(), UsageError> {
    if let Some((decl_ident, decl_vis)) = already_used.get_name(&module.path, decl_name) {
        if !ignore_visibility && decl_vis < min_vis {
            return Err(UsageError::Visibility {
                orig: None,
                decl: (module.path.clone(), decl_ident),
                min_vis,
                decl_vis,
            });
        }
    } else {
        if let Some(decl) = module.syntax.global_declarations.iter().find(|decl| {
            decl.ident()
                .is_some_and(|ident| &**ident.name() == decl_name)
        }) {
            // we found a declaration with the right name, let's analyze it.
            if !ignore_visibility && decl.visibility() < min_vis {
                return Err(UsageError::Visibility {
                    orig: None,
                    decl: (
                        module.path.clone(),
                        decl.ident().unwrap(/* SAFETY: condition of the outer if */),
                    ),
                    min_vis,
                    decl_vis: decl.visibility(),
                });
            } else {
                decl_usage_analysis(module, decl, already_used, to_analyze, ignore_visibility)?;
            }
        } else if let Some((decl_ident, item)) = module
            .imports
            .iter()
            .find(|(ident, _)| *ident.name() == decl_name)
        {
            // there is no declaration with this name, but there is a re-export.
            if !ignore_visibility && item.visibility < min_vis {
                return Err(UsageError::Visibility {
                    orig: None,
                    decl: (module.path.clone(), decl_ident.clone()),
                    min_vis,
                    decl_vis: item.visibility,
                });
            } else {
                to_analyze.insert_ident(item.path.clone(), item.ident.clone(), item.visibility);
            }
        } else {
            return Err(UsageError::NotFound(
                module.path.clone(),
                decl_name.to_string(),
            ));
        }
    }
    Ok(())
}

/// Find identifiers used by a declaration.
fn decl_usage_analysis(
    module: &Module,
    decl: &GlobalDeclaration,
    already_used: &mut UsedItems,
    to_analyze: &mut UsedItems,
    ignore_visibility: bool,
) -> Result<(), UsageError> {
    if decl.ident().is_some_and(|ident| {
        !already_used.insert_ident(module.path.clone(), ident, decl.visibility())
    }) {
        // the ident has already been analyzed.
        return Ok(());
    }

    // inside visit_rec below we can't simply exit the function so we mutate this instead.
    let mut res = Ok(());

    Visit::<TypeExpression>::visit_rec(decl, &mut |ty_expr| {
        let imported: Vec<_> = match imported_item_path(ty_expr, &module.path, &module.imports) {
            Some(item) => vec![item],
            None => module
                .wildcard_items(ty_expr)
                .iter()
                .map(|item| (item.path.clone(), item.ident.clone()))
                .collect(),
        };

        // if this ident refers an imported item, we add it to the list of used items.
        if !imported.is_empty() {
            for (import_path, import_ident) in imported {
                if let Err(err) = import_usage_analysis(
                    module,
                    decl,
                    import_path,
                    import_ident,
                    already_used,
                    to_analyze,
                    ignore_visibility,
                ) {
                    res = Err(err);
                }
            }
        }
        // this ident refers a local declaration, we analyze it recursively.
        else {
            // look up a global decl with the same ident - not just the same name,
            // because it could be shadowed by a local decl
            let decl = module
                .syntax
                .global_declarations
                .iter()
                .find(|decl| decl.ident().is_some_and(|ident| ident == ty_expr.ident));

            if let Some(decl) = decl
                && let Err(err) =
                    decl_usage_analysis(module, decl, already_used, to_analyze, ignore_visibility)
            {
                res = Err(err);
            }
        }
    });

    res
}

/// Record that `decl` uses the item `import_ident` of the module `import_path`.
fn import_usage_analysis(
    module: &Module,
    decl: &GlobalDeclaration,
    import_path: ModulePath,
    import_ident: Ident,
    already_used: &mut UsedItems,
    to_analyze: &mut UsedItems,
    ignore_visibility: bool,
) -> Result<(), UsageError> {
    let min_vis = required_visibility(&module.path, &import_path);
    if let Some((decl_ident, decl_vis)) = already_used.get_name(&import_path, &import_ident.name())
    {
        if !ignore_visibility && decl_vis < min_vis {
            return Err(UsageError::Visibility {
                orig: decl.ident().map(|ident| (module.path.clone(), ident)),
                decl: (import_path, decl_ident),
                min_vis,
                decl_vis,
            });
        }
    } else {
        to_analyze.insert_ident(import_path, import_ident, min_vis);
    }
    Ok(())
}

#[test]
fn test_ignore_visibility() {
    let module = Module::new(
        "library".parse().unwrap(),
        "private fn value() {}".parse().unwrap(),
    );
    for ignore_visibility in [false, true] {
        assert_eq!(
            usage_analysis(
                &module,
                "value",
                Visibility::Public,
                &mut UsedItems::new(),
                &mut UsedItems::new(),
                ignore_visibility,
            )
            .is_ok(),
            ignore_visibility,
        );
    }
}

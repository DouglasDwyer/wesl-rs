use std::collections::{HashMap, HashSet};

use wgsl_parse::{
    SyntaxNode,
    syntax::{Ident, ModulePath, TranslationUnit, Visibility},
};

use super::usage_analysis::import_min_visibility;
use crate::{
    SyntaxUtil,
    error::{Diagnostic, Error},
    pass::{self, CompileResult, CompilerDriver, ImportResolution, Module, UsedItems},
    resolver::{AsyncResolver, Resolver},
};

pub fn main_entry_points(main_module: &TranslationUnit) -> HashSet<Ident> {
    main_module.entry_points().collect()
}

/// Note: it does not call [`pass::retarget_idents`], because that must be done right after [`pass::condcomp`].
pub fn load_module(path: &ModulePath, resolver: &impl Resolver) -> Result<TranslationUnit, Error> {
    let source = resolver.resolve_source(path)?;

    let module: TranslationUnit = source.parse().map_err(|e| {
        Diagnostic::from(e)
            .with_module_path(path.clone(), resolver.display_name(path))
            .with_source(source.to_string())
    })?;

    Ok(module)
}

pub async fn load_module_async(
    path: &ModulePath,
    resolver: &impl AsyncResolver,
) -> Result<TranslationUnit, Error> {
    let source = resolver.resolve_source_async(path).await?;

    let mut module: TranslationUnit = source.parse().map_err(|e| {
        Diagnostic::from(e)
            .with_module_path(path.clone(), resolver.display_name(path))
            .with_source(source.to_string())
    })?;

    pass::retarget_idents(&mut module);

    Ok(module)
}

/// Run usage analysis from `to_analyze` until a fixed point, loading missing modules.
fn analyze(
    driver: &mut impl CompilerDriver,
    modules: &mut Vec<Module>,
    used_items: &mut UsedItems,
    mut to_analyze: UsedItems,
) -> Result<(), Error> {
    loop {
        let mut next_to_analyze = UsedItems::new();

        for (path, items_to_analyze) in to_analyze.iter() {
            let path = driver.canonical_path(path);
            let module = match modules.iter().find(|module| module.path == path) {
                Some(module) => module,
                None => {
                    let module = driver.load_module(&path)?;
                    modules.push_mut(Module::new(path, module))
                }
            };

            driver.module_usage_analysis(module, used_items, &mut next_to_analyze)?;

            for (item, min_vis) in items_to_analyze {
                driver.usage_analysis(
                    module,
                    &item.name(),
                    *min_vis,
                    used_items,
                    &mut next_to_analyze,
                )?;
            }
        }

        if next_to_analyze.is_empty() {
            return Ok(());
        }

        to_analyze = next_to_analyze;
    }
}

/// Async version of [`analyze`].
async fn analyze_async(
    driver: &mut impl CompilerDriver,
    modules: &mut Vec<Module>,
    used_items: &mut UsedItems,
    mut to_analyze: UsedItems,
) -> Result<(), Error> {
    loop {
        let mut next_to_analyze = UsedItems::new();

        for (path, items_to_analyze) in to_analyze.iter() {
            let path = driver.canonical_path(path);
            let module = match modules.iter().find(|module| module.path == path) {
                Some(module) => module,
                None => {
                    let module = driver.load_module_async(&path).await?;
                    modules.push_mut(Module::new(path, module))
                }
            };

            driver.module_usage_analysis(module, used_items, &mut next_to_analyze)?;

            for (item, min_vis) in items_to_analyze {
                driver.usage_analysis(
                    module,
                    &item.name(),
                    *min_vis,
                    used_items,
                    &mut next_to_analyze,
                )?;
            }
        }

        if next_to_analyze.is_empty() {
            return Ok(());
        }

        to_analyze = next_to_analyze;
    }
}

/// List every declaration and every import of every loaded module.
///
/// An import resolves to an item if the imported module is loaded and declares or
/// re-exports that name. Otherwise it resolves to a module, and is listed so it gets loaded.
fn eager_seeds(driver: &impl CompilerDriver, modules: &[Module]) -> UsedItems {
    let mut items = UsedItems::new();
    for module in modules {
        for ident in module
            .syntax
            .global_declarations
            .iter()
            .filter_map(|decl| decl.ident())
        {
            items.insert_ident(module.path.clone(), ident, Visibility::Private);
        }

        for item in module.imports.values() {
            let path = driver.canonical_path(&item.path);
            let name = item.ident.name();
            let min_vis = import_min_visibility(&path, &module.path);
            match modules.iter().find(|m| m.path == path) {
                None => {
                    items.insert_module(path, Default::default());
                }
                Some(target) => {
                    let is_item = target.imports.iter().any(|(i, it)| {
                        *i.name() == *name && !(it.path == target.path && *it.ident.name() == *name)
                    }) || target
                        .syntax
                        .global_declarations
                        .iter()
                        .any(|decl| decl.ident().is_some_and(|i| *i.name() == *name));
                    if is_item {
                        items.insert_ident(path, item.ident.clone(), min_vis);
                    } else {
                        let mut submodule = path;
                        submodule.push(&name);
                        items.insert_module(submodule, Default::default());
                    }
                }
            }
        }
    }
    items
}

/// List loaded modules that have not been usage-analyzed in `used_items` yet.
fn unregistered_modules(modules: &[Module], used_items: &UsedItems) -> UsedItems {
    let mut items = UsedItems::new();
    for module in modules {
        if !used_items.contains_module(&module.path) {
            items.insert_module(module.path.clone(), Default::default());
        }
    }
    items
}

/// Load all modules referenced by any declaration or import, used or not.
///
/// Modules found this way are registered in `used_items` without marking any of their
/// declarations as used, so stripping is unaffected.
fn analyze_eager(
    driver: &mut impl CompilerDriver,
    modules: &mut Vec<Module>,
    used_items: &mut UsedItems,
) -> Result<(), Error> {
    let mut eager_used = UsedItems::new();
    loop {
        let loaded = modules.len();
        let seeds = eager_seeds(driver, modules);
        analyze(driver, modules, &mut eager_used, seeds)?;
        if modules.len() == loaded {
            return Ok(());
        }
        let new_modules = unregistered_modules(modules, used_items);
        analyze(driver, modules, used_items, new_modules)?;
    }
}

/// Async version of [`analyze_eager`].
async fn analyze_eager_async(
    driver: &mut impl CompilerDriver,
    modules: &mut Vec<Module>,
    used_items: &mut UsedItems,
) -> Result<(), Error> {
    let mut eager_used = UsedItems::new();
    loop {
        let loaded = modules.len();
        let seeds = eager_seeds(driver, modules);
        analyze_async(driver, modules, &mut eager_used, seeds).await?;
        if modules.len() == loaded {
            return Ok(());
        }
        let new_modules = unregistered_modules(modules, used_items);
        analyze_async(driver, modules, used_items, new_modules).await?;
    }
}

/// Default implementation of [`CompilerDriver::compile`]
pub fn compile(driver: &mut impl CompilerDriver) -> Result<CompileResult, Error> {
    let main_path = driver.main_path().clone();
    let main_module = driver.load_module(&main_path)?;
    let main_entrypoints = driver
        .main_entry_points(&main_module)?
        .into_iter()
        .map(|ident| (ident, Visibility::Private)) // No visibility requirements for entry points
        .collect::<HashMap<Ident, Visibility>>();

    let mut modules = Vec::new();
    modules.push(Module::new(main_path.clone(), main_module));

    let mut used_items = UsedItems::new();
    let mut to_analyze = UsedItems::new();
    to_analyze.insert_module(main_path, main_entrypoints);

    analyze(driver, &mut modules, &mut used_items, to_analyze)?;

    if driver.import_resolution() == ImportResolution::Eager {
        analyze_eager(driver, &mut modules, &mut used_items)?;
    }

    let final_module = driver.link(&mut modules, &used_items)?;

    Ok(CompileResult {
        syntax: final_module,
        modules,
        used_items,
    })
}

/// Async version of [`compile`].
pub async fn compile_async(driver: &mut impl CompilerDriver) -> Result<CompileResult, Error> {
    let main_path = driver.main_path().clone();
    let main_module = driver.load_module(&main_path)?;
    let main_entrypoints = driver
        .main_entry_points(&main_module)?
        .into_iter()
        .map(|ident| (ident, Visibility::Private)) // No visibility requirements for entry points
        .collect::<HashMap<Ident, Visibility>>();

    let mut modules = Vec::new();
    modules.push(Module::new(main_path.clone(), main_module));

    let mut used_items = UsedItems::new();
    let mut to_analyze = UsedItems::new();
    to_analyze.insert_module(main_path, main_entrypoints);

    analyze_async(driver, &mut modules, &mut used_items, to_analyze).await?;

    if driver.import_resolution() == ImportResolution::Eager {
        analyze_eager_async(driver, &mut modules, &mut used_items).await?;
    }

    let final_module = driver.link(&mut modules, &used_items)?;

    Ok(CompileResult {
        syntax: final_module,
        modules,
        used_items,
    })
}

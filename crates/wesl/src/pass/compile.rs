use std::collections::{HashMap, HashSet};

use itertools::Itertools;
use wgsl_parse::syntax::{Ident, ModulePath, TranslationUnit, Visibility};

use crate::{
    SyntaxUtil,
    error::{Diagnostic, Error, Warning},
    pass::{self, CompileResult, CompilerDriver, LoadedModules, Module, UsedItems},
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

/// Canonical paths of the wildcard imported modules of `module` that are not loaded yet.
fn unloaded_wildcard_targets(
    driver: &impl CompilerDriver,
    module: &Module,
    modules: &[Module],
    peeked: &HashMap<ModulePath, Module>,
) -> Vec<ModulePath> {
    module
        .wildcards
        .iter()
        .map(|wildcard| driver.canonical_path(&wildcard.path))
        .filter(|path| {
            *path != module.path
                && !modules.iter().any(|m| m.path == *path)
                && !peeked.contains_key(path)
        })
        .unique()
        .collect()
}

/// Add a freshly loaded module to the modules in use.
///
/// The modules it wildcard imports are loaded into `peeked`, so their items are known. They
/// are only used by the compilation (and no `const_assert` is included) when referenced.
fn add_module(
    driver: &mut impl CompilerDriver,
    mut module: Module,
    modules: &mut Vec<Module>,
    peeked: &mut HashMap<ModulePath, Module>,
    warnings: &mut Vec<Warning>,
) -> Result<(), Error> {
    for path in unloaded_wildcard_targets(driver, &module, modules, peeked) {
        let syntax = driver.load_module(&path)?;
        peeked.insert(path.clone(), Module::new(path, syntax));
    }

    let loaded = LoadedModules::new(modules, peeked);
    warnings.extend(driver.resolve_wildcards(&mut module, &loaded)?);
    modules.push(module);
    Ok(())
}

/// Async version of [`add_module`].
async fn add_module_async(
    driver: &mut impl CompilerDriver,
    mut module: Module,
    modules: &mut Vec<Module>,
    peeked: &mut HashMap<ModulePath, Module>,
    warnings: &mut Vec<Warning>,
) -> Result<(), Error> {
    for path in unloaded_wildcard_targets(driver, &module, modules, peeked) {
        let syntax = driver.load_module_async(&path).await?;
        peeked.insert(path.clone(), Module::new(path, syntax));
    }

    let loaded = LoadedModules::new(modules, peeked);
    warnings.extend(driver.resolve_wildcards(&mut module, &loaded)?);
    modules.push(module);
    Ok(())
}

/// Register the modules loaded only to read wildcard imported items, without any used item.
///
/// This way users of [`UsedItems`] know that the result depends on them.
fn record_peeked_modules(used_items: &mut UsedItems, peeked: HashMap<ModulePath, Module>) {
    for path in peeked.into_keys() {
        used_items.insert_module(path, HashMap::new());
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
    let mut peeked = HashMap::new();
    let mut warnings = Vec::new();
    add_module(
        driver,
        Module::new(main_path.clone(), main_module),
        &mut modules,
        &mut peeked,
        &mut warnings,
    )?;

    let mut used_items = UsedItems::new();
    let mut to_analyze = UsedItems::new();
    to_analyze.insert_module(main_path, main_entrypoints);

    loop {
        let mut next_to_analyze = UsedItems::new();

        for (path, items_to_analyze) in to_analyze.iter() {
            let path = driver.canonical_path(path);
            if !modules.iter().any(|module| module.path == path) {
                let module = match peeked.remove(&path) {
                    Some(module) => module,
                    None => Module::new(path.clone(), driver.load_module(&path)?),
                };
                add_module(driver, module, &mut modules, &mut peeked, &mut warnings)?;
            }
            let module = modules
                .iter()
                .find(|module| module.path == path)
                .unwrap(/* SAFETY: the module was just added */);

            driver.module_usage_analysis(module, &mut used_items, &mut next_to_analyze)?;

            for (item, min_vis) in items_to_analyze {
                driver.usage_analysis(
                    module,
                    &item.name(),
                    *min_vis,
                    &mut used_items,
                    &mut next_to_analyze,
                )?;
            }
        }

        if next_to_analyze.is_empty() {
            break;
        }

        to_analyze = next_to_analyze;
    }

    record_peeked_modules(&mut used_items, peeked);

    let final_module = driver.link(&mut modules, &used_items)?;

    Ok(CompileResult {
        syntax: final_module,
        modules,
        used_items,
        warnings,
    })
}

pub async fn compile_async(driver: &mut impl CompilerDriver) -> Result<CompileResult, Error> {
    let main_path = driver.main_path().clone();
    let main_module = driver.load_module(&main_path)?;
    let main_entrypoints = driver
        .main_entry_points(&main_module)?
        .into_iter()
        .map(|ident| (ident, Visibility::Private)) // No visibility requirements for entry points
        .collect::<HashMap<Ident, Visibility>>();

    let mut modules = Vec::new();
    let mut peeked = HashMap::new();
    let mut warnings = Vec::new();
    add_module_async(
        driver,
        Module::new(main_path.clone(), main_module),
        &mut modules,
        &mut peeked,
        &mut warnings,
    )
    .await?;

    let mut used_items = UsedItems::new();
    let mut to_analyze = UsedItems::new();
    to_analyze.insert_module(main_path, main_entrypoints);

    loop {
        let mut next_to_analyze = UsedItems::new();

        for (path, items_to_analyze) in to_analyze.iter() {
            let path = driver.canonical_path(path);
            if !modules.iter().any(|module| module.path == path) {
                let module = match peeked.remove(&path) {
                    Some(module) => module,
                    None => Module::new(path.clone(), driver.load_module_async(&path).await?),
                };
                add_module_async(driver, module, &mut modules, &mut peeked, &mut warnings).await?;
            }
            let module = modules
                .iter()
                .find(|module| module.path == path)
                .unwrap(/* SAFETY: the module was just added */);

            driver.module_usage_analysis(module, &mut used_items, &mut next_to_analyze)?;

            for (item, min_vis) in items_to_analyze {
                driver.usage_analysis(
                    module,
                    &item.name(),
                    *min_vis,
                    &mut used_items,
                    &mut next_to_analyze,
                )?;
            }
        }

        if next_to_analyze.is_empty() {
            break;
        }

        to_analyze = next_to_analyze;
    }

    record_peeked_modules(&mut used_items, peeked);

    let final_module = driver.link(&mut modules, &used_items)?;

    Ok(CompileResult {
        syntax: final_module,
        modules,
        used_items,
        warnings,
    })
}

//! Low-level reusable compilation passes.

mod compile;
mod condcomp;
mod driver;
mod import;
mod link;
mod lower;
mod mangle;
mod retarget_idents;
mod usage_analysis;
mod validate;
mod visibility;
mod visit;
mod wildcard;

pub use compile::{compile, compile_async, load_module, load_module_async, main_entry_points};
pub use condcomp::{Feature, Features, condcomp};
pub use driver::{CompileResult, CompilerDriver};
pub use import::{
    ImportedItem, Imports, WildcardImport, flatten_imports, imported_item_path, suppressed_rules,
};
pub use link::link;
pub use lower::lower;
pub use mangle::mangle;
pub use retarget_idents::{retarget_idents, retarget_idents_with, retarget_modules};
pub use usage_analysis::{Module, UsedItems, module_usage_analysis, usage_analysis};
pub use validate::{validate_wesl, validate_wgsl};
pub use visibility::strip_visibility;
pub use visit::Visit;
pub use wildcard::{
    LoadedModules, builtin_shadow_warnings, has_wildcards, is_wildcardable, resolve_wildcards,
    strip_wesl_diagnostics,
};

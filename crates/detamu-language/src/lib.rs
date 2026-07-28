//! Programming-language extensions for the Detamu code world model.
//!
//! LSP, tree-sitter, build metadata, and external analysis engines are possible
//! observer implementations. Language is not a Detamu kernel concept.

use std::sync::Arc;

use detamu_model::ModelAnalyzer;
use detamu_model_code::LanguageId;

pub trait LanguagePack: Send + Sync {
    fn language(&self) -> LanguageId;

    fn extensions(&self) -> &[&str];

    fn analyzers(&self) -> Vec<Arc<dyn ModelAnalyzer>>;
}

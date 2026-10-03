//! Deterministic, seeded generation of training pairs.
//!
//! [`PairFactory`] is the abstract factory; each target language/dialect is an
//! implementation (see [`factories::ruby_rbs`]). [`Store`] persists pairs as
//! JSONL so the output can feed a training pipeline directly.

pub mod factories;
pub mod github;
pub mod highlight;
pub mod lsp;
pub mod pair;
pub mod rng;
pub mod settings;
pub mod store;
pub mod ui;

pub use pair::{Pair, PairFactory};
pub use settings::Settings;
pub use store::Store;

/// All factories available to the GUI / CLI, configured from `settings`.
pub fn registry(settings: &Settings) -> Vec<Box<dyn PairFactory>> {
    vec![Box::new(factories::ruby_rbs::RubyRbsFactory::new(
        settings.sentinel.clone(),
        settings.lsp.clone(),
    ))]
}

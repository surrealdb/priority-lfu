#![allow(clippy::needless_doctest_main)]
#![doc = include_str!("../../../README.md")]

mod builder;
mod cache;
mod deepsize;
mod erased;
mod guard;
mod metrics;
mod shard;
mod traits;

pub use builder::CacheBuilder;
pub use cache::Cache;
pub use deepsize::{Context, DeepSizeOf};
pub use guard::Guard;
pub use metrics::CacheMetrics;
pub use traits::{CacheKey, CachePolicy};
pub use weighted_cache_derive::*;

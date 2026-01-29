#![allow(clippy::needless_doctest_main)]
#![doc = include_str!("../README.md")]

mod builder;
mod cache;
mod deepsize;
mod erased;
mod guard;
#[cfg(feature = "metrics")]
mod metrics;
mod shard;
mod traits;

pub use builder::CacheBuilder;
pub use cache::Cache;
pub use deepsize::{Context, DeepSizeOf};
pub use guard::Guard;
#[cfg(feature = "metrics")]
pub use metrics::CacheMetrics;
pub use traits::{CacheKey, CachePolicy};
pub use priority_lfu_derive::*;

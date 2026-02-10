#![allow(clippy::needless_doctest_main)]
#![doc = include_str!("../README.md")]

mod builder;
mod cache;
mod deepsize;
mod erased;
mod guard;
mod lifecycle;
#[cfg(feature = "metrics")]
mod metrics;
mod shard;
mod traits;

pub use builder::CacheBuilder;
pub use cache::Cache;
pub use deepsize::{Context, DeepSizeOf};
pub use guard::Guard;
pub use lifecycle::{DefaultLifecycle, Lifecycle, TypedLifecycle};
#[cfg(feature = "metrics")]
pub use metrics::CacheMetrics;
pub use priority_lfu_derive::*;
pub use traits::{CacheKey, CacheKeyLookup, CachePolicy};

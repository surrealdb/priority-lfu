//! # Weighted Cache
//!
//! A high-performance, concurrent, in-memory cache with:
//! - **Size-bounded capacity** (bytes, not item count)
//! - **Weight-based eviction** (separate from size)
//! - **Heterogeneous storage** (multiple key/value types without a unified enum)
//! - **W-TinyLFU eviction policy** for near-optimal hit rates
//! - **Read-optimized concurrency** via fine-grained sharding
//!
//! ## Quick Start
//!
//! ```rust
//! use weighted_cache::{DeepSizeOf, Cache, CacheKey, CacheValue};
//! use std::sync::Arc;
//!
//! // Define your key type
//! #[derive(Hash, Eq, PartialEq, Clone)]
//! struct UserId(u64);
//!
//! // Define your value type
//! #[derive(Clone, Debug, PartialEq, DeepSizeOf)]
//! struct UserProfile {
//!     name: String,
//!     email: String,
//! }
//!
//! // Implement the required traits
//! impl CacheKey for UserId {
//!     type Value = UserProfile;
//! }
//!
//! impl CacheValue for UserProfile {
//!     fn deep_size(&self) -> usize {
//!         std::mem::size_of::<Self>()
//!             + self.name.capacity()
//!             + self.email.capacity()
//!     }
//!
//!     fn weight(&self) -> u64 {
//!         100 // Higher weight = more resistant to eviction
//!     }
//! }
//!
//! // Create a cache with 1GB capacity
//! let cache = Cache::new(1024 * 1024 * 1024);
//!
//! // Insert a value
//! let user = UserProfile {
//!     name: "Alice".to_string(),
//!     email: "alice@example.com".to_string(),
//! };
//! cache.insert(UserId(1), user);
//!
//! // Retrieve a value (returns Arc for cheap cloning)
//! if let Some(profile) = cache.get_arc(&UserId(1)) {
//!     println!("User: {}", profile.name);
//! }
//! ```
//!
//! ## Async Usage
//!
//! The cache is safe to use in async contexts. Use `get_arc()` to avoid
//! holding locks across await points:
//!
//! ```rust,ignore
//! use std::sync::Arc;
//!
//! async fn process_user(cache: Arc<Cache>, user_id: UserId) {
//!     // ✅ Safe: Arc is returned immediately, lock is released
//!     if let Some(profile) = cache.get_arc(&user_id) {
//!         // Can safely await while holding the Arc
//!         expensive_async_operation(&profile).await;
//!     }
//! }
//! ```
//!
//! ## Thread Safety
//!
//! The cache is `Send + Sync` and can be shared across threads via `Arc`:
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use std::thread;
//!
//! let cache = Arc::new(Cache::new(1024 * 1024));
//!
//! let handles: Vec<_> = (0..4)
//!     .map(|i| {
//!         let cache = cache.clone();
//!         thread::spawn(move || {
//!             cache.insert(key, value);
//!         })
//!     })
//!     .collect();
//!
//! for handle in handles {
//!     handle.join().unwrap();
//! }
//! ```

mod builder;
mod cache;
mod erased;
mod guard;
mod segments;
mod shard;
mod sketch;
mod traits;

pub use builder::CacheBuilder;
pub use cache::Cache;
pub use deepsize::DeepSizeOf;
pub use guard::Guard;
pub use traits::{CacheKey, CacheValue};

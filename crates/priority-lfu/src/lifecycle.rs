//! Lifecycle hooks for cache items.
//!
//! The [`Lifecycle`] trait allows you to hook into cache item lifecycle events,
//! such as eviction. This is useful for maintaining external indexes or performing
//! cleanup when items are removed from the cache.
//!
//! # Example
//!
//! ```
//! use std::any::Any;
//! use priority_lfu::{Lifecycle, CacheBuilder, CacheKey, DeepSizeOf};
//!
//! #[derive(Hash, Eq, PartialEq, Clone)]
//! struct MyKey(u64);
//!
//! #[derive(Clone, DeepSizeOf)]
//! struct MyValue(String);
//!
//! impl CacheKey for MyKey {
//!     type Value = MyValue;
//! }
//!
//! // Lifecycle that tracks evicted keys
//! #[derive(Clone)]
//! struct MyLifecycle;
//!
//! impl Lifecycle for MyLifecycle {
//!     fn on_evict(&self, key: &dyn Any) {
//!         if let Some(key) = key.downcast_ref::<MyKey>() {
//!             println!("Evicted key: {}", key.0);
//!         }
//!     }
//! }
//!
//! let cache = CacheBuilder::new(1024)
//!     .lifecycle(MyLifecycle)
//!     .build();
//! ```

use std::any::Any;
use std::sync::Arc;

/// Hooks into the lifetime of cache items.
///
/// The functions should be small and very fast, otherwise the cache performance
/// might be negatively affected. Lifecycle methods are called synchronously after
/// releasing the shard lock.
///
/// # Type Erasure
///
/// Because the cache supports heterogeneous key types via type erasure, the lifecycle
/// methods receive type-erased `&dyn Any` references. Use `downcast_ref` to recover
/// the concrete types:
///
/// ```ignore
/// fn on_evict(&self, key: &dyn Any) {
///     if let Some(key) = key.downcast_ref::<MyKey>() {
///         // Handle eviction of MyKey
///     }
/// }
/// ```
pub trait Lifecycle: Send + Sync {
	/// Called when an item is evicted from the cache due to capacity pressure.
	///
	/// This is called during eviction triggered by insert operations that need space.
	///
	/// # Arguments
	///
	/// * `key` - Reference to the evicted key (downcast to your key type)
	fn on_evict(&self, key: &dyn Any);

	/// Called when an item is explicitly removed via `cache.remove()`.
	///
	/// By default, this calls [`on_evict`]. Override if you need different behavior
	/// for explicit removals vs automatic evictions.
	///
	/// # Arguments
	///
	/// * `key` - Reference to the removed key (downcast to your key type)
	fn on_remove(&self, key: &dyn Any) {
		self.on_evict(key);
	}

	/// Called when the cache is cleared via `cache.clear()`.
	///
	/// By default, this calls [`on_evict`] for each entry. Override if you need
	/// bulk cleanup logic instead of per-entry callbacks.
	///
	/// # Arguments
	///
	/// * `key` - Reference to the cleared key (downcast to your key type)
	fn on_clear(&self, key: &dyn Any) {
		self.on_evict(key);
	}
}

/// Default lifecycle implementation that does nothing.
///
/// This is used when no lifecycle hooks are needed.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultLifecycle;

impl Lifecycle for DefaultLifecycle {
	#[inline]
	fn on_evict(&self, _key: &dyn Any) {
		// No-op
	}

	#[inline]
	fn on_remove(&self, _key: &dyn Any) {
		// No-op
	}

	#[inline]
	fn on_clear(&self, _key: &dyn Any) {
		// No-op
	}
}

/// Lifecycle implementation that wraps an `Arc<L>` for shared ownership.
///
/// This is used internally when the cache needs to clone the lifecycle.
impl<L: Lifecycle> Lifecycle for Arc<L> {
	#[inline]
	fn on_evict(&self, key: &dyn Any) {
		(**self).on_evict(key);
	}

	#[inline]
	fn on_remove(&self, key: &dyn Any) {
		(**self).on_remove(key);
	}

	#[inline]
	fn on_clear(&self, key: &dyn Any) {
		(**self).on_clear(key);
	}
}

/// Helper for creating typed lifecycle implementations.
///
/// This wrapper provides convenient typed access to lifecycle events when you only
/// use a single key type in your cache.
///
/// # Example
///
/// ```
/// use priority_lfu::{TypedLifecycle, CacheBuilder, CacheKey, DeepSizeOf};
/// use std::sync::atomic::{AtomicUsize, Ordering};
/// use std::sync::Arc;
///
/// #[derive(Hash, Eq, PartialEq, Clone)]
/// struct MyKey(u64);
///
/// #[derive(Clone, DeepSizeOf)]
/// struct MyValue(String);
///
/// impl CacheKey for MyKey {
///     type Value = MyValue;
/// }
///
/// let evict_count = Arc::new(AtomicUsize::new(0));
/// let counter = evict_count.clone();
///
/// let lifecycle = TypedLifecycle::<MyKey, _>::new(move |_key| {
///     counter.fetch_add(1, Ordering::Relaxed);
/// });
///
/// let cache = CacheBuilder::new(1024)
///     .lifecycle(lifecycle)
///     .build();
/// ```
pub struct TypedLifecycle<K, F>
where
	F: Fn(K) + Send + Sync,
{
	callback: F,
	_marker: std::marker::PhantomData<fn(K)>,
}

impl<K, F> TypedLifecycle<K, F>
where
	F: Fn(K) + Send + Sync,
{
	/// Create a new typed lifecycle with the given eviction callback.
	pub fn new(callback: F) -> Self {
		Self {
			callback,
			_marker: std::marker::PhantomData,
		}
	}
}

impl<K, F> Lifecycle for TypedLifecycle<K, F>
where
	K: Clone + Send + Sync + 'static,
	F: Fn(K) + Send + Sync,
{
	fn on_evict(&self, key: &dyn Any) {
		// Try to downcast key to the expected type
		if let Some(key) = key.downcast_ref::<K>() {
			(self.callback)(key.clone());
		}
	}
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicUsize, Ordering};

	use super::*;

	#[test]
	fn test_default_lifecycle() {
		let lifecycle = DefaultLifecycle;

		// Should not panic
		lifecycle.on_evict(&42u64);
		lifecycle.on_remove(&42u64);
		lifecycle.on_clear(&42u64);
	}

	#[test]
	fn test_typed_lifecycle() {
		let counter = Arc::new(AtomicUsize::new(0));
		let counter_clone = counter.clone();

		let lifecycle = TypedLifecycle::<u64, _>::new(move |key| {
			assert_eq!(key, 42);
			counter_clone.fetch_add(1, Ordering::Relaxed);
		});

		// Matching types should call the callback
		lifecycle.on_evict(&42u64);
		assert_eq!(counter.load(Ordering::Relaxed), 1);

		// Non-matching key type should not call the callback
		lifecycle.on_evict(&"wrong_type");
		assert_eq!(counter.load(Ordering::Relaxed), 1);
	}

	#[test]
	fn test_arc_lifecycle() {
		let counter = Arc::new(AtomicUsize::new(0));
		let counter_clone = counter.clone();

		let lifecycle = TypedLifecycle::<u64, _>::new(move |_| {
			counter_clone.fetch_add(1, Ordering::Relaxed);
		});

		let arc_lifecycle = Arc::new(lifecycle);

		arc_lifecycle.on_evict(&42u64);
		assert_eq!(counter.load(Ordering::Relaxed), 1);
	}
}

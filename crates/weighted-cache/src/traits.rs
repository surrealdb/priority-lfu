use std::hash::Hash;

use crate::deepsize::DeepSizeOf;

/// Cache eviction policy determining retention priority.
///
/// Lower discriminant values = higher priority = evicted last.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum CachePolicy {
	/// Critical metadata that should persist (catalog, schemas, indexes).
	/// Last to be evicted, highest retention.
	Critical = 0,
	
	/// Important data with high reuse (active transaction records, hot tables).
	/// Strong retention, evicted reluctantly.
	Durable = 1,
	
	/// Standard cacheable data (recent queries, lookup results).
	/// Normal eviction behavior.
	#[default]
	Standard = 2,
	
	/// Temporary or easily recomputed data (intermediate results, aggregations).
	/// First to be evicted.
	Volatile = 3,
}

// Implement DeepSizeOf for CachePolicy (zero-sized, stored inline)
impl DeepSizeOf for CachePolicy {
	fn deep_size_of_children(&self, _context: &mut crate::deepsize::Context) -> usize {
		0 // Enum is Copy and has no heap allocations
	}
}

/// Marker trait for cache keys. Associates a key type with its value type.
///
/// # Example
///
/// ```
/// use weighted_cache::{DeepSizeOf, CacheKey, CachePolicy};
///
/// #[derive(Hash, Eq, PartialEq, Clone)]
/// struct UserId(u64);
///
/// #[derive(Clone, Debug, PartialEq, DeepSizeOf)]
/// struct UserData {
///     name: String,
/// }
///
/// impl CacheKey for UserId {
///     type Value = UserData;
///
///     fn policy(&self) -> CachePolicy {
///         CachePolicy::Standard
///     }
/// }
/// ```
pub trait CacheKey: Hash + Eq + Clone + Send + Sync + 'static {
	/// The value type associated with this key.
	type Value: DeepSizeOf + Send + Sync;

	/// Eviction policy determining retention priority.
	///
	/// This is *not* the memory size—it's a logical priority.
	/// When the cache is full, items are evicted in order from Volatile to Critical.
	/// Within each policy tier, a clock algorithm with frequency counting determines
	/// which specific entries are evicted.
	///
	/// This method is called on the key, allowing different keys to have
	/// different policies for the same value type.
	fn policy(&self) -> CachePolicy {
		CachePolicy::Standard
	}
}

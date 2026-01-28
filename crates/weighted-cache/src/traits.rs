use std::hash::Hash;

use weighted_cache_derive::DeepSizeOf;

use crate::deepsize::DeepSizeOf;

pub(crate) const NUM_POLICY_BUCKETS: usize = 3;

/// Cache eviction policy determining retention priority.
///
/// Lower discriminant values = higher priority = evicted last.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, DeepSizeOf)]
pub enum CachePolicy {
	/// Critical metadata that should persist (catalog, schemas, indexes).
	/// Last to be evicted, highest retention.
	Critical = 0,

	/// Standard cacheable data (recent queries, lookup results).
	/// Normal eviction behavior.
	#[default]
	Standard = 1,

	/// Temporary or easily recomputed data (intermediate results, aggregations).
	/// First to be evicted.
	Volatile = 2,
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
	/// This method is called on the key, allowing different keys to have
	/// different policies for the same value type.
	fn policy(&self) -> CachePolicy {
		CachePolicy::Standard
	}
}

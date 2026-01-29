use std::hash::Hash;

use priority_lfu_derive::DeepSizeOf;

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
/// use priority_lfu::{DeepSizeOf, CacheKey, CachePolicy};
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

/// Trait for borrowed keys that can look up entries of type `K`.
///
/// This trait enables zero-allocation cache lookups using borrowed key types.
/// For example, you can use `(&str, &str)` to look up entries stored with
/// `(String, String)` keys, avoiding the allocation of owned strings.
///
/// # Hash Consistency Requirement
///
/// **CRITICAL**: The `Hash` implementation MUST produce the same hash as `K`
/// for equivalent keys. If the hashes differ, lookups will fail.
///
/// # Example
///
/// ```
/// use std::hash::{Hash, Hasher};
/// use priority_lfu::{CacheKey, CacheKeyLookup, CachePolicy, DeepSizeOf};
///
/// // Owned key type
/// #[derive(Hash, Eq, PartialEq, Clone)]
/// struct DbCacheKey(String, String);
///
/// impl CacheKey for DbCacheKey {
///     type Value = String;
/// }
///
/// // Borrowed lookup type
/// struct DbCacheKeyRef<'a>(&'a str, &'a str);
///
/// impl Hash for DbCacheKeyRef<'_> {
///     fn hash<H: Hasher>(&self, state: &mut H) {
///         // MUST match DbCacheKey's hash implementation
///         self.0.hash(state);
///         self.1.hash(state);
///     }
/// }
///
/// impl CacheKeyLookup<DbCacheKey> for DbCacheKeyRef<'_> {
///     fn eq_key(&self, key: &DbCacheKey) -> bool {
///         self.0 == key.0 && self.1 == key.1
///     }
/// }
/// ```
pub trait CacheKeyLookup<K: CacheKey>: Hash {
	/// Check equality against an owned key.
	///
	/// Returns `true` if this borrowed key is equivalent to the given owned key.
	fn eq_key(&self, key: &K) -> bool;
}

/// Blanket implementation: every `CacheKey` can look up itself.
///
/// This allows existing code using `cache.get(&key)` to continue working unchanged.
impl<K: CacheKey> CacheKeyLookup<K> for K {
	fn eq_key(&self, key: &K) -> bool {
		self == key
	}
}

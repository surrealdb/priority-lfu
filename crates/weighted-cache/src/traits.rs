use std::hash::Hash;

use deepsize::DeepSizeOf;

/// Marker trait for cache keys. Associates a key type with its value type.
///
/// # Example
///
/// ```
/// use weighted_cache::{DeepSizeOf, CacheKey};
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
///     fn weight(&self) -> u64 {
///         100
///     }
/// }
/// ```
pub trait CacheKey: Hash + Eq + Clone + Send + Sync + 'static {
	/// The value type associated with this key.
	type Value: DeepSizeOf + Send + Sync;

	/// Eviction priority. Higher weight = more resistant to eviction.
	///
	/// This is *not* the memory size—it's a logical priority.
	/// When the cache is full, items with lower `weight() / frequency` ratios
	/// are evicted first.
	///
	/// This method is called on the key, allowing different keys to have
	/// different weights for the same value type.
	fn weight(&self) -> u64;
}

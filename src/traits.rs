use std::hash::Hash;

use deepsize::DeepSizeOf;

/// Marker trait for cache keys. Associates a key type with its value type.
///
/// # Example
///
/// ```
/// use weighted_cache::{DeepSizeOf, CacheKey, CacheValue};
///
/// #[derive(Hash, Eq, PartialEq, Clone)]
/// struct UserId(u64);
///
/// #[derive(Clone, Debug, PartialEq, DeepSizeOf)]
/// struct UserData {
///     name: String,
/// }
///
/// impl CacheValue for UserData {}
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
	type Value: CacheValue;

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

/// Trait for cacheable values.
///
/// Implementors must provide methods to calculate memory footprint and eviction priority.
///
/// # Example
///
/// ```
/// use weighted_cache::{DeepSizeOf, CacheValue};
///
/// #[derive(Clone, Debug, PartialEq, DeepSizeOf)]
/// struct UserData {
///     name: String,
///     email: String,
/// }
///
/// impl CacheValue for UserData {
///     fn deep_size(&self) -> usize {
///         // Approximate memory usage
///         std::mem::size_of::<Self>()
///             + self.name.capacity()
///             + self.email.capacity()
///     }
/// }
/// ```
pub trait CacheValue: DeepSizeOf + Send + Sync + 'static {
	/// Memory footprint in bytes. Called once on insertion.
	///
	/// This should account for heap-allocated memory owned by the value.
	/// For simple types, use `std::mem::size_of::<Self>()`.
	/// For complex types with heap allocations (Vec, String, etc.),
	/// add their capacity to the total.
	fn deep_size(&self) -> usize {
		DeepSizeOf::deep_size_of(self)
	}
}

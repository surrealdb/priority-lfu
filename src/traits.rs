use std::hash::Hash;

/// Marker trait for cache keys. Associates a key type with its value type.
///
/// # Example
///
/// ```
/// use weighted_cache::CacheKey;
///
/// #[derive(Hash, Eq, PartialEq, Clone)]
/// struct UserId(u64);
///
/// impl CacheKey for UserId {
///     type Value = UserData;
/// }
/// ```
pub trait CacheKey: Hash + Eq + Clone + Send + Sync + 'static {
	/// The value type associated with this key.
	type Value: CacheValue;
}

/// Trait for cacheable values.
///
/// Implementors must provide methods to calculate memory footprint and eviction priority.
///
/// # Example
///
/// ```
/// use weighted_cache::CacheValue;
///
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
///
///     fn weight(&self) -> u64 {
///         // Higher weight = more resistant to eviction
///         100
///     }
/// }
/// ```
pub trait CacheValue: Send + Sync + 'static {
	/// Memory footprint in bytes. Called once on insertion.
	///
	/// This should account for heap-allocated memory owned by the value.
	/// For simple types, use `std::mem::size_of::<Self>()`.
	/// For complex types with heap allocations (Vec, String, etc.),
	/// add their capacity to the total.
	fn deep_size(&self) -> usize;

	/// Eviction priority. Higher weight = more resistant to eviction.
	///
	/// This is *not* the memory size—it's a logical priority.
	/// When the cache is full, items with lower `weight() / frequency` ratios
	/// are evicted first.
	fn weight(&self) -> u64;
}

use crate::cache::Cache;

/// Builder for configuring a Cache.
///
/// # Example
///
/// ```
/// use priority_lfu::CacheBuilder;
///
/// let cache = CacheBuilder::new(1024 * 1024 * 512) // 512 MB
///     .shards(128)
///     .build();
/// ```
///
/// # Automatic Shard Scaling
///
/// By default, the cache uses up to 64 shards, but automatically scales down
/// for smaller caches to ensure each shard has at least 4KB capacity. This
/// prevents premature eviction due to uneven hash distribution.
///
/// - 256KB+ capacity: 64 shards (4KB+ per shard)
/// - 64KB capacity: 16 shards (4KB per shard)
/// - 4KB capacity: 1 shard (4KB per shard)
///
/// You can override this with [`shards()`], but the count may still be reduced
/// if the capacity is too small to support the requested number.
pub struct CacheBuilder {
	max_size: usize,
	shard_count: Option<usize>,
}

impl CacheBuilder {
	/// Create a new builder with the given maximum size in bytes.
	pub fn new(max_size_bytes: usize) -> Self {
		Self {
			max_size: max_size_bytes,
			shard_count: None,
		}
	}

	/// Set the number of shards.
	///
	/// More shards reduce contention but increase memory overhead.
	/// Will be rounded up to the next power of 2.
	///
	/// **Note**: The shard count may be reduced if the capacity is too small to
	/// support the requested number of shards (minimum 4KB per shard). This prevents
	/// premature eviction due to uneven hash distribution.
	///
	/// Default: up to 64 shards, scaled based on capacity
	pub fn shards(mut self, count: usize) -> Self {
		self.shard_count = Some(count);
		self
	}

	/// Build the cache with the configured settings.
	pub fn build(self) -> Cache {
		match self.shard_count {
			Some(count) => Cache::with_shards(self.max_size, count),
			None => Cache::new(self.max_size),
		}
	}
}

impl Default for CacheBuilder {
	/// Create a builder with default settings and 1GB capacity.
	fn default() -> Self {
		Self::new(1024 * 1024 * 1024) // 1 GB
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_builder_default() {
		let cache = CacheBuilder::new(1024).build();
		assert!(cache.is_empty());
	}

	#[test]
	fn test_builder_with_shards() {
		let cache = CacheBuilder::new(1024).shards(32).build();
		assert!(cache.is_empty());
	}

	#[test]
	fn test_builder_full_config() {
		let cache = CacheBuilder::new(10240).shards(32).build();

		assert!(cache.is_empty());
		assert_eq!(cache.size(), 0);
	}
}

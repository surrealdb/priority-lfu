use crate::cache::Cache;

/// Builder for configuring a Cache.
///
/// # Example
///
/// ```
/// use weighted_cache::CacheBuilder;
///
/// let cache = CacheBuilder::new(1024 * 1024 * 512) // 512 MB
///     .shards(128)
///     .build();
/// ```
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
	/// Default: 64 shards
	pub fn shards(mut self, count: usize) -> Self {
		self.shard_count = Some(count);
		self
	}

	/// Build the cache with the configured settings.
	pub fn build(self) -> Cache {
		let shard_count = self.shard_count.unwrap_or(64);
		Cache::with_shards(self.max_size, shard_count)
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
		let cache = CacheBuilder::new(10240)
			.shards(32)
			.build();

		assert!(cache.is_empty());
		assert_eq!(cache.size(), 0);
	}
}

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
///     .hot_percent(0.95)  // 95% hot target instead of default 90%
///     .build();
/// ```
pub struct CacheBuilder {
	max_size: usize,
	shard_count: Option<usize>,
	hot_percent: f32,
}

impl CacheBuilder {
	/// Create a new builder with the given maximum size in bytes.
	pub fn new(max_size_bytes: usize) -> Self {
		Self {
			max_size: max_size_bytes,
			shard_count: None,
			hot_percent: 0.9, // Default: 90% hot target
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

	/// Set the hot target size as a percentage of total capacity.
	///
	/// The hot ring holds frequently accessed items.
	/// Valid range: 0.5 to 0.99 (50% to 99%).
	///
	/// Default: 0.9 (90%)
	pub fn hot_percent(mut self, percent: f32) -> Self {
		assert!(percent > 0.5 && percent < 1.0, "hot_percent must be between 0.5 and 1.0");
		self.hot_percent = percent;
		self
	}

	/// Build the cache with the configured settings.
	pub fn build(self) -> Cache {
		let shard_count = self.shard_count.unwrap_or(64);
		Cache::with_config(self.max_size, shard_count, self.hot_percent)
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
	fn test_builder_with_hot_percent() {
		let cache = CacheBuilder::new(1024).hot_percent(0.95).build();
		assert!(cache.is_empty());
	}

	#[test]
	#[should_panic(expected = "hot_percent must be between")]
	fn test_builder_invalid_hot_percent() {
		CacheBuilder::new(1024).hot_percent(0.3).build();
	}

	#[test]
	fn test_builder_full_config() {
		// Test that custom percentages are accepted and cache is created
		let cache = CacheBuilder::new(10240)
			.shards(32)
			.hot_percent(0.85)
			.build();

		assert!(cache.is_empty());
		assert_eq!(cache.size(), 0);
	}
}

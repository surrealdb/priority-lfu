use crate::cache::Cache;

/// Builder for configuring a Cache.
///
/// # Example
///
/// ```ignore
/// use weighted_cache::CacheBuilder;
///
/// let cache = CacheBuilder::new(1024 * 1024 * 512) // 512 MB
///     .shards(128)
///     .window_percent(0.02)  // 2% instead of default 1%
///     .build();
/// ```
pub struct CacheBuilder {
	max_size: usize,
	shard_count: Option<usize>,
	window_percent: f32,
	protected_percent: f32,
}

impl CacheBuilder {
	/// Create a new builder with the given maximum size in bytes.
	pub fn new(max_size_bytes: usize) -> Self {
		Self {
			max_size: max_size_bytes,
			shard_count: None,
			window_percent: 0.01,    // Default: 1%
			protected_percent: 0.80, // Default: 80% of main cache
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

	/// Set the window size as a percentage of total capacity.
	///
	/// The window is a small LRU that captures recently inserted items.
	/// Valid range: 0.001 to 0.1 (0.1% to 10%).
	///
	/// Default: 0.01 (1%)
	pub fn window_percent(mut self, percent: f32) -> Self {
		assert!(percent > 0.0 && percent < 0.2, "window_percent must be between 0 and 0.2");
		self.window_percent = percent;
		self
	}

	/// Set the protected segment size as a percentage of the main cache.
	///
	/// The main cache is divided into probationary and protected segments.
	/// Valid range: 0.5 to 0.95 (50% to 95%).
	///
	/// Default: 0.80 (80%)
	pub fn protected_percent(mut self, percent: f32) -> Self {
		assert!(percent > 0.5 && percent < 1.0, "protected_percent must be between 0.5 and 1.0");
		self.protected_percent = percent;
		self
	}

	/// Build the cache with the configured settings.
	pub fn build(self) -> Cache {
		let shard_count = self.shard_count.unwrap_or(64);

		// For now, we use the simplified Cache::with_shards constructor
		// In a full implementation, we'd pass all the custom percentages
		// TODO: Extend Cache to accept custom window/protected percentages
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
	fn test_builder_with_window_percent() {
		let cache = CacheBuilder::new(1024).window_percent(0.05).build();
		assert!(cache.is_empty());
	}

	#[test]
	#[should_panic(expected = "window_percent must be between")]
	fn test_builder_invalid_window_percent() {
		CacheBuilder::new(1024).window_percent(0.5).build();
	}

	#[test]
	#[should_panic(expected = "protected_percent must be between")]
	fn test_builder_invalid_protected_percent() {
		CacheBuilder::new(1024).protected_percent(0.3).build();
	}
}

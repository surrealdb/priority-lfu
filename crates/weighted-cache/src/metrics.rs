//! Cache performance metrics.

/// Performance metrics for the cache.
///
/// This struct provides insights into cache behavior including hit rates,
/// eviction counts, and memory utilization.
///
/// # Example
///
/// ```
/// use weighted_cache::Cache;
///
/// let cache = Cache::new(1024 * 1024);
/// // ... perform cache operations ...
///
/// let metrics = cache.metrics();
/// println!("Hit rate: {:.2}%", metrics.hit_rate() * 100.0);
/// println!("Utilization: {:.2}%", metrics.utilization() * 100.0);
/// println!("Evictions: {}", metrics.evictions);
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct CacheMetrics {
	/// Number of successful cache lookups (get/get_arc).
	pub hits: u64,
	/// Number of failed cache lookups (key not found).
	pub misses: u64,
	/// Number of new entries inserted into the cache.
	pub inserts: u64,
	/// Number of existing entries updated (key already existed).
	pub updates: u64,
	/// Number of entries evicted due to capacity constraints.
	pub evictions: u64,
	/// Number of entries explicitly removed via remove().
	pub removals: u64,
	/// Current total size in bytes across all entries.
	pub current_size_bytes: usize,
	/// Maximum capacity in bytes.
	pub capacity_bytes: usize,
	/// Current number of entries in the cache.
	pub entry_count: usize,
}

impl CacheMetrics {
	/// Calculate the cache hit rate as a ratio between 0.0 and 1.0.
	///
	/// Returns 0.0 if there have been no cache accesses.
	///
	/// # Example
	///
	/// ```
	/// # use weighted_cache::Cache;
	/// # let cache = Cache::new(1024);
	/// let metrics = cache.metrics();
	/// let hit_rate = metrics.hit_rate();
	/// if hit_rate < 0.5 {
	///     println!("Cache hit rate is low: {:.2}%", hit_rate * 100.0);
	/// }
	/// ```
	pub fn hit_rate(&self) -> f64 {
		let total = self.hits + self.misses;
		if total == 0 {
			0.0
		} else {
			self.hits as f64 / total as f64
		}
	}

	/// Calculate memory utilization as a ratio between 0.0 and 1.0.
	///
	/// Returns the fraction of capacity currently in use.
	///
	/// # Example
	///
	/// ```
	/// # use weighted_cache::Cache;
	/// # let cache = Cache::new(1024);
	/// let metrics = cache.metrics();
	/// let utilization = metrics.utilization();
	/// if utilization > 0.9 {
	///     println!("Cache is {:.1}% full", utilization * 100.0);
	/// }
	/// ```
	pub fn utilization(&self) -> f64 {
		if self.capacity_bytes == 0 {
			0.0
		} else {
			self.current_size_bytes as f64 / self.capacity_bytes as f64
		}
	}

	/// Calculate the total number of cache accesses (hits + misses).
	///
	/// # Example
	///
	/// ```
	/// # use weighted_cache::Cache;
	/// # let cache = Cache::new(1024);
	/// let metrics = cache.metrics();
	/// println!("Total accesses: {}", metrics.total_accesses());
	/// ```
	pub fn total_accesses(&self) -> u64 {
		self.hits + self.misses
	}

	/// Calculate the total number of write operations (inserts + updates).
	///
	/// # Example
	///
	/// ```
	/// # use weighted_cache::Cache;
	/// # let cache = Cache::new(1024);
	/// let metrics = cache.metrics();
	/// println!("Total writes: {}", metrics.total_writes());
	/// ```
	pub fn total_writes(&self) -> u64 {
		self.inserts + self.updates
	}
}

#[cfg(feature = "metrics")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::RwLock;

use crate::erased::{Entry, ErasedKey, ErasedKeyLookup, ErasedKeyRef};
use crate::guard::Guard;
#[cfg(feature = "metrics")]
use crate::metrics::CacheMetrics;
use crate::shard::Shard;
use crate::traits::{CacheKey, CacheKeyLookup};

/// Thread-safe cache with weight-stratified clock eviction.
///
/// The cache can be shared across threads via `Arc<Cache>`. All methods are synchronous
/// but safe to call from async contexts.
///
/// # Weight-Stratified Clock Eviction
///
/// This cache uses a two-level eviction strategy that combines policy-based prioritization
/// with recency and frequency tracking:
///
/// 1. **Policy-based stratification**: Each entry has an eviction policy (Critical, Standard, or
///    Volatile). When space is needed, lower-priority entries are evicted first:
///    - **Volatile** (lowest priority) - evicted first
///    - **Standard** (normal priority) - evicted if Volatile is empty
///    - **Critical** (highest priority) - evicted only as a last resort
///
/// 2. **Clock algorithm**: Within each policy bucket, a "clock sweep" finds eviction candidates.
///    The algorithm uses two signals:
///    - **Clock bit**: Set when an entry is accessed. During eviction, if set, the bit is cleared
///      and the entry gets another chance. If clear, the entry is a candidate.
///    - **Frequency counter** (0-255): Tracks access frequency. Even with a clear clock bit,
///      high-frequency entries get additional chances via frequency decay.
///
/// This approach ensures that frequently-accessed entries resist eviction regardless of policy,
/// while still respecting policy-based priorities during memory pressure.
///
/// # Sharding for Concurrency
///
/// The cache divides its capacity across multiple shards (up to 64 by default). Each shard
/// has its own lock, reducing contention in concurrent workloads. Keys are distributed to
/// shards via their hash value.
///
/// The shard count is automatically scaled based on capacity to ensure each shard has at
/// least 4KB of space. This prevents premature eviction due to uneven hash distribution:
///
/// - Large caches (>= 256KB): 64 shards
/// - Smaller caches: Scaled down proportionally (e.g., 64KB → 16 shards, 4KB → 1 shard)
///
/// # Async Usage
///
/// ```ignore
/// let cache = Arc::new(Cache::new(1024 * 1024));
///
/// // Share across tasks
/// let cache_clone = cache.clone();
/// tokio::spawn(async move {
///     // Use get_clone() in async contexts to avoid holding guards across await points
///     if let Some(value) = cache_clone.get_clone(&key) {
///         do_async_work(&value).await;
///     }
/// });
/// ```
pub struct Cache {
	/// Sharded storage
	shards: Vec<RwLock<Shard>>,
	/// Current total size in bytes
	current_size: AtomicUsize,
	/// Total entry count
	entry_count: AtomicUsize,
	/// Number of shards
	shard_count: usize,
	/// Maximum capacity in bytes
	#[cfg_attr(not(feature = "metrics"), allow(dead_code))]
	max_size_bytes: usize,
	/// Metrics: cache hits
	#[cfg(feature = "metrics")]
	hits: AtomicU64,
	/// Metrics: cache misses
	#[cfg(feature = "metrics")]
	misses: AtomicU64,
	/// Metrics: new inserts
	#[cfg(feature = "metrics")]
	inserts: AtomicU64,
	/// Metrics: updates (replaced existing key)
	#[cfg(feature = "metrics")]
	updates: AtomicU64,
	/// Metrics: evictions
	#[cfg(feature = "metrics")]
	evictions: AtomicU64,
	/// Metrics: explicit removals
	#[cfg(feature = "metrics")]
	removals: AtomicU64,
}

/// Minimum size per shard in bytes.
///
/// This ensures each shard has enough capacity to hold a reasonable number of entries,
/// preventing premature eviction due to hash distribution variance across shards.
/// With 4KB minimum, a shard can comfortably hold dozens of typical cache entries.
const MIN_SHARD_SIZE: usize = 4096;

/// Default number of shards for large caches.
///
/// More shards reduce contention but increase memory overhead.
/// This value is used when capacity is large enough to support it.
const DEFAULT_SHARD_COUNT: usize = 64;

/// Compute the optimal shard count for a given capacity.
///
/// This ensures each shard has at least `MIN_SHARD_SIZE` bytes to prevent
/// premature eviction due to uneven hash distribution.
fn compute_shard_count(capacity: usize, desired_shards: usize) -> usize {
	// Calculate max shards that maintain MIN_SHARD_SIZE per shard
	let max_shards = (capacity / MIN_SHARD_SIZE).max(1);

	// Use the smaller of desired and max, ensuring power of 2
	desired_shards.min(max_shards).next_power_of_two().max(1)
}

impl Cache {
	/// Create a new cache with the given maximum size in bytes.
	///
	/// Uses default configuration with automatic shard scaling. The number of shards
	/// is chosen to balance concurrency (more shards = less contention) with per-shard
	/// capacity (each shard needs enough space to avoid premature eviction).
	///
	/// - Large caches (>= 256KB): 64 shards
	/// - Smaller caches: Scaled down to ensure at least 4KB per shard
	///
	/// For explicit control over shard count, use [`CacheBuilder`] or [`with_shards`].
	pub fn new(max_size_bytes: usize) -> Self {
		let shard_count = compute_shard_count(max_size_bytes, DEFAULT_SHARD_COUNT);
		Self::with_shards_internal(max_size_bytes, shard_count)
	}

	/// Create with custom shard count.
	///
	/// More shards reduce contention but increase memory overhead.
	/// Recommended: `num_cpus * 8` to `num_cpus * 16`.
	///
	/// **Note**: The shard count may be reduced if the capacity is too small to
	/// support the requested number of shards (minimum 4KB per shard). This prevents
	/// premature eviction due to uneven hash distribution.
	pub fn with_shards(max_size_bytes: usize, shard_count: usize) -> Self {
		let shard_count = compute_shard_count(max_size_bytes, shard_count);
		Self::with_shards_internal(max_size_bytes, shard_count)
	}

	/// Internal constructor that uses the shard count directly (already validated).
	fn with_shards_internal(max_size_bytes: usize, shard_count: usize) -> Self {
		// Divide capacity per shard
		let size_per_shard = max_size_bytes / shard_count;

		// Create shards
		let shards = (0..shard_count).map(|_| RwLock::new(Shard::new(size_per_shard))).collect();

		Self {
			shards,
			current_size: AtomicUsize::new(0),
			entry_count: AtomicUsize::new(0),
			shard_count,
			max_size_bytes,
			#[cfg(feature = "metrics")]
			hits: AtomicU64::new(0),
			#[cfg(feature = "metrics")]
			misses: AtomicU64::new(0),
			#[cfg(feature = "metrics")]
			inserts: AtomicU64::new(0),
			#[cfg(feature = "metrics")]
			updates: AtomicU64::new(0),
			#[cfg(feature = "metrics")]
			evictions: AtomicU64::new(0),
			#[cfg(feature = "metrics")]
			removals: AtomicU64::new(0),
		}
	}

	/// Insert a key-value pair. Evicts items if necessary.
	///
	/// Returns the previous value if the key existed.
	///
	/// # Runtime Complexity
	///
	/// Expected case: O(1) for successful insertion without eviction.
	///
	/// Worst case: O(n) where n is the number of entries per shard.
	/// Eviction happens via clock hand advancement within the shard.
	pub fn insert<K: CacheKey>(&self, key: K, value: K::Value) -> Option<K::Value> {
		let erased_key = ErasedKey::new(&key);
		let policy = key.policy();
		let entry = Entry::new(value, policy);
		let entry_size = entry.size;

		// Get the shard
		let shard_lock = self.get_shard(erased_key.hash);

		// Acquire write lock
		let mut shard = shard_lock.write();

		// Insert (handles eviction internally via Clock-PRO)
		let (old_entry, (num_evictions, evicted_size)) = shard.insert(erased_key, entry);

		if let Some(ref old) = old_entry {
			// Update size (might be different)
			let size_diff = entry_size as isize - old.size as isize;
			if size_diff > 0 {
				self.current_size.fetch_add(size_diff as usize, Ordering::Relaxed);
			} else {
				self.current_size.fetch_sub((-size_diff) as usize, Ordering::Relaxed);
			}
			// Metrics: track update
			#[cfg(feature = "metrics")]
			self.updates.fetch_add(1, Ordering::Relaxed);
		} else {
			// New entry
			self.current_size.fetch_add(entry_size, Ordering::Relaxed);
			self.entry_count.fetch_add(1, Ordering::Relaxed);
			// Metrics: track insert
			#[cfg(feature = "metrics")]
			self.inserts.fetch_add(1, Ordering::Relaxed);
		}

		// Account for evictions
		if num_evictions > 0 {
			self.entry_count.fetch_sub(num_evictions, Ordering::Relaxed);
			self.current_size.fetch_sub(evicted_size, Ordering::Relaxed);
			// Metrics: track evictions
			#[cfg(feature = "metrics")]
			self.evictions.fetch_add(num_evictions as u64, Ordering::Relaxed);
		}

		old_entry.and_then(|e| e.into_value::<K::Value>())
	}

	/// Retrieve a value via guard. The guard holds a read lock on the shard.
	///
	/// # Warning
	///
	/// Do NOT hold this guard across `.await` points. Use `get_clone()` instead
	/// for async contexts.
	///
	/// # Runtime Complexity
	///
	/// Expected case: O(1)
	///
	/// This method performs a zero-allocation hash table lookup and atomically
	/// increments the reference counter for Clock-PRO.
	pub fn get<K: CacheKey>(&self, key: &K) -> Option<Guard<'_, K::Value>> {
		let key_ref = ErasedKeyRef::new(key);
		let shard_lock = self.get_shard(key_ref.hash);

		// Acquire read lock
		let shard = shard_lock.read();

		// Look up entry (bumps reference counter internally, zero allocation)
		let Some(entry) = shard.get_ref(&key_ref) else {
			// Metrics: track miss
			#[cfg(feature = "metrics")]
			self.misses.fetch_add(1, Ordering::Relaxed);
			return None;
		};

		// Metrics: track hit
		#[cfg(feature = "metrics")]
		self.hits.fetch_add(1, Ordering::Relaxed);

		// Get pointer to value (use value_ref for type-safe downcast)
		let value_ref = entry.value_ref::<K::Value>()?;
		let value_ptr = value_ref as *const K::Value;

		// SAFETY: We hold a read lock on the shard, so the entry won't be modified
		// or dropped while the guard exists. The guard ties its lifetime to the lock.
		unsafe { Some(Guard::new(shard, value_ptr)) }
	}

	/// Retrieve a cloned value. Safe to hold across `.await` points.
	///
	/// Requires `V: Clone`. This is the preferred method for async code.
	///
	/// # Runtime Complexity
	///
	/// Expected case: O(1) + O(m) where m is the cost of cloning the value.
	///
	/// This method performs a hash table lookup in O(1) expected time, then clones
	/// the underlying value. If cloning is expensive, consider using `Arc<T>` as your
	/// value type, which makes cloning O(1).
	pub fn get_clone<K: CacheKey>(&self, key: &K) -> Option<K::Value>
	where
		K::Value: Clone,
	{
		let key_ref = ErasedKeyRef::new(key);
		let shard_lock = self.get_shard(key_ref.hash);

		// Short-lived read lock
		let shard = shard_lock.read();

		let Some(entry) = shard.get_ref(&key_ref) else {
			// Metrics: track miss
			#[cfg(feature = "metrics")]
			self.misses.fetch_add(1, Ordering::Relaxed);
			return None;
		};

		// Metrics: track hit
		#[cfg(feature = "metrics")]
		self.hits.fetch_add(1, Ordering::Relaxed);

		// Clone the value
		entry.value_ref::<K::Value>().cloned()
	}

	/// Retrieve a value via guard using a borrowed lookup key (zero allocation).
	///
	/// This method allows looking up entries using a borrowed key type `Q` that
	/// implements `CacheKeyLookup<K>`, enabling zero-allocation lookups. For example,
	/// you can use `(&str, &str)` to look up entries stored with `(String, String)` keys.
	///
	/// # Warning
	///
	/// Do NOT hold this guard across `.await` points. Use `get_clone_by()` instead
	/// for async contexts.
	///
	/// # Example
	///
	/// ```ignore
	/// // Define borrowed lookup type
	/// struct DbCacheKeyRef<'a>(&'a str, &'a str);
	///
	/// impl Hash for DbCacheKeyRef<'_> {
	///     fn hash<H: Hasher>(&self, state: &mut H) {
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
	///
	/// // Zero-allocation lookup
	/// let value = cache.get_by::<DbCacheKey, _>(&DbCacheKeyRef(ns, db));
	/// ```
	pub fn get_by<K, Q>(&self, key: &Q) -> Option<Guard<'_, K::Value>>
	where
		K: CacheKey,
		Q: CacheKeyLookup<K> + ?Sized,
	{
		let key_ref = ErasedKeyLookup::new(key);
		let shard_lock = self.get_shard(key_ref.hash);

		// Acquire read lock
		let shard = shard_lock.read();

		// Look up entry (bumps reference counter internally, zero allocation)
		let Some(entry) = shard.get_ref_by(&key_ref) else {
			// Metrics: track miss
			#[cfg(feature = "metrics")]
			self.misses.fetch_add(1, Ordering::Relaxed);
			return None;
		};

		// Metrics: track hit
		#[cfg(feature = "metrics")]
		self.hits.fetch_add(1, Ordering::Relaxed);

		// Get pointer to value (use value_ref for type-safe downcast)
		let value_ref = entry.value_ref::<K::Value>()?;
		let value_ptr = value_ref as *const K::Value;

		// SAFETY: We hold a read lock on the shard, so the entry won't be modified
		// or dropped while the guard exists. The guard ties its lifetime to the lock.
		unsafe { Some(Guard::new(shard, value_ptr)) }
	}

	/// Retrieve a cloned value using a borrowed lookup key. Safe to hold across `.await` points.
	///
	/// This method allows looking up entries using a borrowed key type `Q` that
	/// implements `CacheKeyLookup<K>`, enabling zero-allocation lookups. For example,
	/// you can use `(&str, &str)` to look up entries stored with `(String, String)` keys.
	///
	/// Requires `V: Clone`. This is the preferred method for async code.
	///
	/// # Example
	///
	/// ```ignore
	/// // Zero-allocation lookup with cloned result
	/// let value = cache.get_clone_by::<DbCacheKey, _>(&DbCacheKeyRef(ns, db));
	/// ```
	pub fn get_clone_by<K, Q>(&self, key: &Q) -> Option<K::Value>
	where
		K: CacheKey,
		K::Value: Clone,
		Q: CacheKeyLookup<K> + ?Sized,
	{
		let key_ref = ErasedKeyLookup::new(key);
		let shard_lock = self.get_shard(key_ref.hash);

		// Short-lived read lock
		let shard = shard_lock.read();

		let Some(entry) = shard.get_ref_by(&key_ref) else {
			// Metrics: track miss
			#[cfg(feature = "metrics")]
			self.misses.fetch_add(1, Ordering::Relaxed);
			return None;
		};

		// Metrics: track hit
		#[cfg(feature = "metrics")]
		self.hits.fetch_add(1, Ordering::Relaxed);

		// Clone the value
		entry.value_ref::<K::Value>().cloned()
	}

	/// Remove a key from the cache.
	///
	/// # Runtime Complexity
	///
	/// Expected case: O(1)
	///
	/// Worst case: O(n) where n is the number of entries per shard.
	///
	/// The worst case occurs when the entry is removed from an IndexMap segment,
	/// which preserves insertion order and may require shifting elements. In practice,
	/// most removals are O(1) amortized.
	pub fn remove<K: CacheKey>(&self, key: &K) -> Option<K::Value> {
		let erased_key = ErasedKey::new(key);
		let shard_lock = self.get_shard(erased_key.hash);

		let mut shard = shard_lock.write();
		let entry = shard.remove(&erased_key)?;

		self.current_size.fetch_sub(entry.size, Ordering::Relaxed);
		self.entry_count.fetch_sub(1, Ordering::Relaxed);

		// Metrics: track removal
		#[cfg(feature = "metrics")]
		self.removals.fetch_add(1, Ordering::Relaxed);

		entry.into_value::<K::Value>()
	}

	/// Check if a key exists (zero allocation).
	pub fn contains<K: CacheKey>(&self, key: &K) -> bool {
		let key_ref = ErasedKeyRef::new(key);
		let shard_lock = self.get_shard(key_ref.hash);
		let shard = shard_lock.read();

		// Use get_ref for zero-allocation lookup
		shard.get_ref(&key_ref).is_some()
	}

	/// Check if a borrowed lookup key exists (zero allocation).
	///
	/// This method allows checking existence using a borrowed key type `Q` that
	/// implements `CacheKeyLookup<K>`, enabling zero-allocation lookups.
	///
	/// # Example
	///
	/// ```ignore
	/// // Zero-allocation existence check
	/// let exists = cache.contains_by::<DbCacheKey, _>(&DbCacheKeyRef(ns, db));
	/// ```
	pub fn contains_by<K, Q>(&self, key: &Q) -> bool
	where
		K: CacheKey,
		Q: CacheKeyLookup<K> + ?Sized,
	{
		let key_ref = ErasedKeyLookup::new(key);
		let shard_lock = self.get_shard(key_ref.hash);
		let shard = shard_lock.read();

		// Use get_ref_by for zero-allocation lookup
		shard.get_ref_by(&key_ref).is_some()
	}

	/// Current total size in bytes.
	pub fn size(&self) -> usize {
		self.current_size.load(Ordering::Relaxed)
	}

	/// Number of entries across all types.
	pub fn len(&self) -> usize {
		self.entry_count.load(Ordering::Relaxed)
	}

	/// Check if cache is empty.
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// Clear all entries.
	///
	/// # Runtime Complexity
	///
	/// O(n) where n is the total number of entries in the cache.
	///
	/// This method acquires a write lock on each shard sequentially and clears
	/// all data structures (HashMap and IndexMaps).
	pub fn clear(&self) {
		for shard_lock in &self.shards {
			let mut shard = shard_lock.write();
			shard.clear();
		}
		self.current_size.store(0, Ordering::Relaxed);
		self.entry_count.store(0, Ordering::Relaxed);

		// Reset all metrics
		#[cfg(feature = "metrics")]
		{
			self.hits.store(0, Ordering::Relaxed);
			self.misses.store(0, Ordering::Relaxed);
			self.inserts.store(0, Ordering::Relaxed);
			self.updates.store(0, Ordering::Relaxed);
			self.evictions.store(0, Ordering::Relaxed);
			self.removals.store(0, Ordering::Relaxed);
		}
	}

	/// Get performance metrics snapshot.
	///
	/// Returns a snapshot of cache performance metrics including hits, misses,
	/// evictions, and memory utilization.
	///
	/// # Example
	///
	/// ```
	/// use priority_lfu::Cache;
	///
	/// let cache = Cache::new(1024 * 1024);
	/// // ... perform cache operations ...
	///
	/// let metrics = cache.metrics();
	/// println!("Hit rate: {:.2}%", metrics.hit_rate() * 100.0);
	/// println!("Cache utilization: {:.2}%", metrics.utilization() * 100.0);
	/// println!("Total evictions: {}", metrics.evictions);
	/// ```
	#[cfg(feature = "metrics")]
	pub fn metrics(&self) -> CacheMetrics {
		CacheMetrics {
			hits: self.hits.load(Ordering::Relaxed),
			misses: self.misses.load(Ordering::Relaxed),
			inserts: self.inserts.load(Ordering::Relaxed),
			updates: self.updates.load(Ordering::Relaxed),
			evictions: self.evictions.load(Ordering::Relaxed),
			removals: self.removals.load(Ordering::Relaxed),
			current_size_bytes: self.current_size.load(Ordering::Relaxed),
			capacity_bytes: self.max_size_bytes,
			entry_count: self.entry_count.load(Ordering::Relaxed),
		}
	}

	/// Get the shard for a given hash.
	fn get_shard(&self, hash: u64) -> &RwLock<Shard> {
		let index = (hash as usize) & (self.shard_count - 1);
		&self.shards[index]
	}
}

// Thread safety: Cache can be shared across threads
unsafe impl Send for Cache {}
unsafe impl Sync for Cache {}

#[cfg(test)]
mod tests {
	use std::borrow::Borrow;

	use super::*;
	use crate::DeepSizeOf;

	#[derive(Hash, Eq, PartialEq, Clone, Debug)]
	struct TestKey(u64);

	impl CacheKey for TestKey {
		type Value = TestValue;
	}

	#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
	struct TestValue {
		data: String,
	}

	#[test]
	fn test_compute_shard_count_scales_with_capacity() {
		// Very small capacity: should get 1 shard
		assert_eq!(compute_shard_count(1024, 64), 1);
		assert_eq!(compute_shard_count(4095, 64), 1);

		// Exactly MIN_SHARD_SIZE: 1 shard
		assert_eq!(compute_shard_count(4096, 64), 1);

		// 2x MIN_SHARD_SIZE: 2 shards (power of 2)
		assert_eq!(compute_shard_count(8192, 64), 2);

		// 16x MIN_SHARD_SIZE: 16 shards
		assert_eq!(compute_shard_count(65536, 64), 16);

		// Large capacity: full 64 shards
		assert_eq!(compute_shard_count(256 * 1024, 64), 64);
		assert_eq!(compute_shard_count(1024 * 1024, 64), 64);

		// Custom shard count is also bounded
		assert_eq!(compute_shard_count(8192, 128), 2); // Can't have 128 shards with 8KB
		assert_eq!(compute_shard_count(1024 * 1024, 128), 128); // Large cache can have 128
	}

	#[test]
	fn test_cache_insert_and_get() {
		let cache = Cache::new(1024);

		let key = TestKey(1);
		let value = TestValue {
			data: "hello".to_string(),
		};

		cache.insert(key.clone(), value.clone());

		let retrieved = cache.get_clone(&key).expect("key should exist");
		assert_eq!(retrieved, value);
	}

	#[test]
	fn test_cache_remove() {
		let cache = Cache::new(1024);

		let key = TestKey(1);
		let value = TestValue {
			data: "hello".to_string(),
		};

		cache.insert(key.clone(), value.clone());
		assert!(cache.contains(&key));

		let removed = cache.remove(&key).expect("key should exist");
		assert_eq!(removed, value);
		assert!(!cache.contains(&key));
	}

	#[test]
	fn test_cache_eviction() {
		// Small cache that will trigger eviction (use fewer shards for small capacity)
		let cache = Cache::with_shards(1000, 4);

		// Insert values that exceed capacity
		for i in 0..15 {
			let key = TestKey(i);
			let value = TestValue {
				data: "x".repeat(50),
			};
			cache.insert(key, value);
		}

		// Cache should have evicted some entries
		assert!(cache.len() < 15, "Cache should have evicted some entries");
		assert!(cache.size() <= 1000, "Cache size should be <= 1000, got {}", cache.size());
	}

	#[test]
	fn test_cache_concurrent_access() {
		use std::sync::Arc;
		use std::thread;

		let cache = Arc::new(Cache::new(10240));
		let mut handles = vec![];

		for t in 0..4 {
			let cache = cache.clone();
			handles.push(thread::spawn(move || {
				for i in 0..100 {
					let key = TestKey(t * 100 + i);
					let value = TestValue {
						data: format!("value-{}", i),
					};
					cache.insert(key.clone(), value.clone());

					if let Some(retrieved) = cache.get_clone(&key) {
						assert_eq!(retrieved, value);
					}
				}
			}));
		}

		for handle in handles {
			handle.join().expect("thread should not panic");
		}

		assert!(!cache.is_empty());
	}

	#[test]
	fn test_cache_is_send_sync() {
		fn assert_send<T: Send>() {}
		fn assert_sync<T: Sync>() {}

		assert_send::<Cache>();
		assert_sync::<Cache>();
	}

	// Tests for borrowed key lookups

	#[derive(Hash, Eq, PartialEq, Clone, Debug)]
	struct DbCacheKey(String, String);

	impl CacheKey for DbCacheKey {
		type Value = TestValue;
	}

	struct DbCacheKeyRef<'a>(&'a str, &'a str);

	impl std::hash::Hash for DbCacheKeyRef<'_> {
		fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
			// MUST match DbCacheKey's hash implementation
			self.0.hash(state);
			self.1.hash(state);
		}
	}

	impl CacheKeyLookup<DbCacheKey> for DbCacheKeyRef<'_> {
		fn eq_key(&self, key: &DbCacheKey) -> bool {
			self.0 == key.0 && self.1 == key.1
		}

		fn to_owned_key(self) -> DbCacheKey {
			DbCacheKey(self.0.to_owned(), self.1.to_owned())
		}
	}

	#[test]
	fn test_borrowed_key_lookup_get_by() {
		let cache = Cache::new(1024);

		let key = DbCacheKey("namespace".to_string(), "database".to_string());
		let value = TestValue {
			data: "test_data".to_string(),
		};

		cache.insert(key.clone(), value.clone());

		// Lookup using borrowed key (zero allocation)
		let borrowed_key = DbCacheKeyRef("namespace", "database");
		let retrieved = cache.get_by::<DbCacheKey, _>(&borrowed_key);
		assert!(retrieved.is_some());
		assert_eq!(*retrieved.unwrap(), value);

		// Lookup with non-existent key
		let borrowed_key_missing = DbCacheKeyRef("namespace", "missing");
		let retrieved = cache.get_by::<DbCacheKey, _>(&borrowed_key_missing);
		assert!(retrieved.is_none());
	}

	#[test]
	fn test_borrowed_key_lookup_get_clone_by() {
		let cache = Cache::new(1024);

		let key = DbCacheKey("ns".to_string(), "db".to_string());
		let value = TestValue {
			data: "cloned_data".to_string(),
		};

		cache.insert(key.clone(), value.clone());

		// Lookup using borrowed key (zero allocation)
		let borrowed_key = DbCacheKeyRef("ns", "db");
		let retrieved = cache.get_clone_by::<DbCacheKey, _>(&borrowed_key);
		assert_eq!(retrieved, Some(value));

		// Lookup with non-existent key
		let borrowed_key_missing = DbCacheKeyRef("ns", "missing");
		let retrieved = cache.get_clone_by::<DbCacheKey, _>(&borrowed_key_missing);
		assert_eq!(retrieved, None);
	}

	#[test]
	fn test_borrowed_key_lookup_contains_by() {
		let cache = Cache::new(1024);

		let key = DbCacheKey("catalog".to_string(), "schema".to_string());
		let value = TestValue {
			data: "contains_test".to_string(),
		};

		cache.insert(key.clone(), value);

		// Check existence using borrowed key (zero allocation)
		let borrowed_key = DbCacheKeyRef("catalog", "schema");
		assert!(cache.contains_by::<DbCacheKey, _>(&borrowed_key));

		// Check non-existent key
		let borrowed_key_missing = DbCacheKeyRef("catalog", "missing");
		assert!(!cache.contains_by::<DbCacheKey, _>(&borrowed_key_missing));
	}

	#[test]
	fn test_borrowed_key_lookup_multiple_entries() {
		let cache = Cache::new(4096);

		// Insert multiple entries
		for i in 0..10 {
			let key = DbCacheKey(format!("ns{}", i), format!("db{}", i));
			let value = TestValue {
				data: format!("data{}", i),
			};
			cache.insert(key, value);
		}

		// Lookup each using borrowed keys
		for i in 0..10 {
			let ns = format!("ns{}", i);
			let db = format!("db{}", i);
			let borrowed_key = DbCacheKeyRef(&ns, &db);

			let retrieved = cache.get_clone_by::<DbCacheKey, _>(&borrowed_key);
			assert!(retrieved.is_some());
			assert_eq!(retrieved.unwrap().data, format!("data{}", i));
		}
	}

	#[test]
	fn test_borrowed_key_existing_api_still_works() {
		let cache = Cache::new(1024);

		let key = DbCacheKey("test".to_string(), "key".to_string());
		let value = TestValue {
			data: "existing_api".to_string(),
		};

		cache.insert(key.clone(), value.clone());

		// Existing API should still work (blanket impl)
		let retrieved = cache.get(&key);
		assert!(retrieved.is_some());
		assert_eq!(*retrieved.unwrap(), value);

		assert!(cache.contains(&key));

		let cloned = cache.get_clone(&key);
		assert_eq!(cloned, Some(value));
	}
}

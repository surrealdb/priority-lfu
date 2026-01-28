use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use parking_lot::RwLock;

use crate::erased::{Entry, ErasedKey, ErasedKeyRef};
use crate::guard::Guard;
use crate::metrics::CacheMetrics;
use crate::shard::Shard;
use crate::traits::CacheKey;

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
/// 1. **Policy-based stratification**: Each entry has an eviction policy (Critical, Standard,
///    or Volatile). When space is needed, lower-priority entries are evicted first:
///    - **Volatile** (lowest priority) - evicted first
///    - **Standard** (normal priority) - evicted if Volatile is empty  
///    - **Critical** (highest priority) - evicted only as a last resort
///
/// 2. **Clock algorithm**: Within each policy bucket, a "clock sweep" finds eviction candidates.
///    The algorithm uses two signals:
///    - **Clock bit**: Set when an entry is accessed. During eviction, if set, the bit is
///      cleared and the entry gets another chance. If clear, the entry is a candidate.
///    - **Frequency counter** (0-255): Tracks access frequency. Even with a clear clock bit,
///      high-frequency entries get additional chances via frequency decay.
///
/// This approach ensures that frequently-accessed entries resist eviction regardless of policy,
/// while still respecting policy-based priorities during memory pressure.
///
/// # Sharding for Concurrency
///
/// The cache divides its capacity across multiple shards (default: 64). Each shard has its
/// own lock, reducing contention in concurrent workloads. Keys are distributed to shards
/// via their hash value.
///
/// # Async Usage
///
/// ```ignore
/// let cache = Arc::new(Cache::new(1024 * 1024));
///
/// // Share across tasks
/// let cache_clone = cache.clone();
/// tokio::spawn(async move {
///     // Use get_arc() in async contexts to avoid holding guards across await points
///     if let Some(value) = cache_clone.get_arc(&key) {
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
	max_size_bytes: usize,
	/// Metrics: cache hits
	hits: AtomicU64,
	/// Metrics: cache misses
	misses: AtomicU64,
	/// Metrics: new inserts
	inserts: AtomicU64,
	/// Metrics: updates (replaced existing key)
	updates: AtomicU64,
	/// Metrics: evictions
	evictions: AtomicU64,
	/// Metrics: explicit removals
	removals: AtomicU64,
}

impl Cache {
	/// Create a new cache with the given maximum size in bytes.
	///
	/// Uses default configuration: 64 shards, 90% hot target.
	pub fn new(max_size_bytes: usize) -> Self {
		Self::with_shards(max_size_bytes, 64)
	}

	/// Create with custom shard count.
	///
	/// More shards reduce contention but increase memory overhead.
	/// Recommended: `num_cpus * 8` to `num_cpus * 16`.
	pub fn with_shards(max_size_bytes: usize, shard_count: usize) -> Self {
		// Ensure shard_count is a power of 2 for efficient masking
		let shard_count = shard_count.next_power_of_two().max(4);

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
			hits: AtomicU64::new(0),
			misses: AtomicU64::new(0),
			inserts: AtomicU64::new(0),
			updates: AtomicU64::new(0),
			evictions: AtomicU64::new(0),
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
	pub fn insert<K: CacheKey>(&self, key: K, value: K::Value) -> Option<Arc<K::Value>> {
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
			self.updates.fetch_add(1, Ordering::Relaxed);
		} else {
			// New entry
			self.current_size.fetch_add(entry_size, Ordering::Relaxed);
			self.entry_count.fetch_add(1, Ordering::Relaxed);
			// Metrics: track insert
			self.inserts.fetch_add(1, Ordering::Relaxed);
		}

		// Account for evictions
		if num_evictions > 0 {
			self.entry_count.fetch_sub(num_evictions, Ordering::Relaxed);
			self.current_size.fetch_sub(evicted_size, Ordering::Relaxed);
			// Metrics: track evictions
			self.evictions.fetch_add(num_evictions as u64, Ordering::Relaxed);
		}

		old_entry.and_then(|e| e.value_arc::<K::Value>())
	}

	/// Retrieve a value via guard. The guard holds a read lock on the shard.
	///
	/// # Warning
	///
	/// Do NOT hold this guard across `.await` points. Use `get_arc()` instead
	/// for async contexts.
	///
	/// # Runtime Complexity
	///
	/// Expected case: O(1)
	///
	/// This method performs a zero-allocation hash table lookup and atomically
	/// increments the reference counter for Clock-PRO.
	pub fn get<K: CacheKey>(&self, key: &K) -> Option<Guard<'_, K::Value>> {
		let key_ref = ErasedKeyRef::new(key); // Zero allocation
		let shard_lock = self.get_shard(key_ref.hash);

		// Acquire read lock
		let shard = shard_lock.read();

		// Look up entry (bumps reference counter internally, zero allocation)
		let Some(entry) = shard.get_ref(&key_ref) else {
			// Metrics: track miss
			self.misses.fetch_add(1, Ordering::Relaxed);
			return None;
		};

		// Metrics: track hit
		self.hits.fetch_add(1, Ordering::Relaxed);

		// Get pointer to value
		let value_ptr = entry.value.as_ref() as *const _ as *const K::Value;

		// SAFETY: We hold a read lock on the shard, so the entry won't be modified
		// or dropped while the guard exists. The guard ties its lifetime to the lock.
		unsafe { Some(Guard::new(shard, value_ptr)) }
	}

	/// Retrieve a value as `Arc<V>`. Safe to hold across `.await` points.
	///
	/// This is the preferred method for async code.
	///
	/// # Runtime Complexity
	///
	/// Expected case: O(1)
	///
	/// This method performs a zero-allocation hash table lookup, clones an `Arc`
	/// pointer (not the underlying value), and atomically increments the reference counter.
	pub fn get_arc<K: CacheKey>(&self, key: &K) -> Option<Arc<K::Value>> {
		let key_ref = ErasedKeyRef::new(key); // Zero allocation
		let shard_lock = self.get_shard(key_ref.hash);

		// Short-lived read lock
		let shard = shard_lock.read();
		let entry = shard.get_ref(&key_ref); // Zero-allocation lookup

		if let Some(entry) = entry {
			// Metrics: track hit
			self.hits.fetch_add(1, Ordering::Relaxed);
			let arc = entry.value_arc::<K::Value>()?;
			Some(arc)
		} else {
			// Metrics: track miss
			self.misses.fetch_add(1, Ordering::Relaxed);
			None
		}
	}

	/// Retrieve a cloned value. Safe to hold across `.await` points.
	///
	/// Requires `V: Clone`. Use `get_arc()` if clone is expensive.
	///
	/// # Runtime Complexity
	///
	/// Expected case: O(1) + O(m) where m is the cost of cloning the value.
	///
	/// This method performs a hash table lookup in O(1) expected time, then clones
	/// the underlying value. If cloning is expensive, prefer `get_arc()` which only
	/// clones the `Arc` pointer in O(1) time.
	pub fn get_clone<K: CacheKey>(&self, key: &K) -> Option<K::Value>
	where
		K::Value: Clone,
	{
		self.get_arc(key).map(|arc| (*arc).clone())
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
	pub fn remove<K: CacheKey>(&self, key: &K) -> Option<Arc<K::Value>> {
		let erased_key = ErasedKey::new(key);
		let shard_lock = self.get_shard(erased_key.hash);

		let mut shard = shard_lock.write();
		let entry = shard.remove(&erased_key)?;

		self.current_size.fetch_sub(entry.size, Ordering::Relaxed);
		self.entry_count.fetch_sub(1, Ordering::Relaxed);

		// Metrics: track removal
		self.removals.fetch_add(1, Ordering::Relaxed);

		entry.value_arc::<K::Value>()
	}

	/// Check if a key exists (zero allocation).
	pub fn contains<K: CacheKey>(&self, key: &K) -> bool {
		let key_ref = ErasedKeyRef::new(key); // Zero allocation
		let shard_lock = self.get_shard(key_ref.hash);
		let shard = shard_lock.read();

		// Use get_ref for zero-allocation lookup
		shard.get_ref(&key_ref).is_some()
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
		self.hits.store(0, Ordering::Relaxed);
		self.misses.store(0, Ordering::Relaxed);
		self.inserts.store(0, Ordering::Relaxed);
		self.updates.store(0, Ordering::Relaxed);
		self.evictions.store(0, Ordering::Relaxed);
		self.removals.store(0, Ordering::Relaxed);
	}

	/// Get performance metrics snapshot.
	///
	/// Returns a snapshot of cache performance metrics including hits, misses,
	/// evictions, and memory utilization.
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
	/// println!("Cache utilization: {:.2}%", metrics.utilization() * 100.0);
	/// println!("Total evictions: {}", metrics.evictions);
	/// ```
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
	fn test_cache_insert_and_get() {
		let cache = Cache::new(1024);

		let key = TestKey(1);
		let value = TestValue {
			data: "hello".to_string(),
		};

		cache.insert(key.clone(), value.clone());

		let retrieved = cache.get_arc(&key).expect("key should exist");
		assert_eq!(*retrieved, value);
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
		assert_eq!(*removed, value);
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

					if let Some(retrieved) = cache.get_arc(&key) {
						assert_eq!(*retrieved, value);
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
}

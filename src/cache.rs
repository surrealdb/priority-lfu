use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_queue::SegQueue;
use parking_lot::RwLock;

use crate::erased::{Entry, ErasedKey, Segment};
use crate::guard::Guard;
use crate::shard::Shard;
use crate::sketch::FrequencySketch;
use crate::traits::CacheKey;

/// Thread-safe cache. Can be shared across threads via `Arc<Cache>`.
///
/// All methods are synchronous but safe to call from async contexts.
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
	/// Global frequency sketch (shared across all shards)
	sketch: FrequencySketch,
	/// Promotion buffer for deferred promotions (lock-free MPMC queue)
	promotion_buffer: SegQueue<ErasedKey>,
	/// Current total size in bytes
	current_size: AtomicUsize,
	/// Total entry count
	entry_count: AtomicUsize,
	/// Maximum size in bytes
	max_size: usize,
	/// Number of shards
	shard_count: usize,
}

impl Cache {
	/// Create a new cache with the given maximum size in bytes.
	///
	/// Uses default configuration: 64 shards, 1% window, 20% probationary, 80% protected.
	pub fn new(max_size_bytes: usize) -> Self {
		Self::with_shards(max_size_bytes, 64)
	}

	/// Create with custom shard count.
	///
	/// More shards reduce contention but increase memory overhead.
	/// Recommended: `num_cpus * 8` to `num_cpus * 16`.
	pub fn with_shards(max_size_bytes: usize, shard_count: usize) -> Self {
		Self::with_config(max_size_bytes, shard_count, 0.01, 0.80)
	}

	/// Create with full custom configuration.
	///
	/// # Arguments
	///
	/// * `max_size_bytes` - Maximum cache size in bytes
	/// * `shard_count` - Number of shards (will be rounded to next power of 2)
	/// * `window_percent` - Window size as percentage of total capacity (e.g., 0.01 = 1%)
	/// * `protected_percent` - Protected segment as percentage of main cache (e.g., 0.80 = 80%)
	///
	/// This is primarily used by `CacheBuilder`. Most users should use `new()` or `with_shards()`.
	pub fn with_config(
		max_size_bytes: usize,
		shard_count: usize,
		window_percent: f32,
		protected_percent: f32,
	) -> Self {
		// Ensure shard_count is a power of 2 for efficient masking
		let shard_count = shard_count.next_power_of_two().max(4);

		// Distribute capacity across segments
		let probationary_percent = 1.0 - protected_percent; // Remaining portion of main cache

		let window_size = (max_size_bytes as f64 * window_percent as f64) as usize;
		let main_size = max_size_bytes - window_size;
		let probationary_size = (main_size as f64 * probationary_percent as f64) as usize;
		let protected_size = (main_size as f64 * protected_percent as f64) as usize;

		// Divide per shard
		let window_per_shard = window_size / shard_count;
		let prob_per_shard = probationary_size / shard_count;
		let prot_per_shard = protected_size / shard_count;

		// Create shards
		let shards = (0..shard_count)
			.map(|_| RwLock::new(Shard::new(window_per_shard, prob_per_shard, prot_per_shard)))
			.collect();

		// Create frequency sketch sized for expected capacity
		let sketch = FrequencySketch::new(max_size_bytes / 100); // Rough estimate

		Self {
			shards,
			sketch,
			promotion_buffer: SegQueue::new(),
			current_size: AtomicUsize::new(0),
			entry_count: AtomicUsize::new(0),
			max_size: max_size_bytes,
			shard_count,
		}
	}

	/// Insert a key-value pair. Evicts items if necessary.
	///
	/// Returns the previous value if the key existed.
	pub fn insert<K: CacheKey>(&self, key: K, value: K::Value) -> Option<Arc<K::Value>> {
		let erased_key = ErasedKey::new(&key);
		let weight = key.weight();
		let entry = Entry::new(value, weight);
		let entry_size = entry.size;

		// Get the shard
		let shard_lock = self.get_shard(erased_key.hash);

		// Process pending promotions before acquiring write lock
		self.drain_promotions();

		// Acquire write lock
		let mut shard = shard_lock.write();

		// Check if key already exists
		let old_entry = shard.insert(erased_key.clone(), entry);

		if let Some(ref old) = old_entry {
			// Update size (might be different)
			let size_diff = entry_size as isize - old.size as isize;
			if size_diff > 0 {
				self.current_size.fetch_add(size_diff as usize, Ordering::Relaxed);
			} else {
				self.current_size.fetch_sub((-size_diff) as usize, Ordering::Relaxed);
			}
		} else {
			// New entry
			self.current_size.fetch_add(entry_size, Ordering::Relaxed);
			self.entry_count.fetch_add(1, Ordering::Relaxed);
		}

		// Increment frequency
		self.sketch.increment(erased_key.hash);

		// Admit from window if needed
		shard.admit_from_window(&self.sketch);

		// Check if we need to evict
		drop(shard); // Release lock before eviction
		while self.current_size.load(Ordering::Relaxed) > self.max_size {
			if !self.evict_one() {
				break; // No more entries to evict
			}
		}

		old_entry.and_then(|e| e.value_arc::<K::Value>())
	}

	/// Retrieve a value via guard. The guard holds a read lock on the shard.
	///
	/// # Warning
	///
	/// Do NOT hold this guard across `.await` points. Use `get_arc()` instead
	/// for async contexts.
	pub fn get<K: CacheKey>(&self, key: &K) -> Option<Guard<'_, K::Value>> {
		let erased_key = ErasedKey::new(key);
		let shard_lock = self.get_shard(erased_key.hash);

		// Acquire read lock
		let shard = shard_lock.read();

		// Look up entry
		let entry = shard.get(&erased_key)?;

		// Increment frequency (lock-free)
		self.sketch.increment(erased_key.hash);

		// Mark for promotion if in probationary
		if entry.get_segment() == Segment::Probationary {
			self.promotion_buffer.push(erased_key);
		}

		// Get pointer to value
		let value_ptr = entry.value.as_ref() as *const _ as *const K::Value;

		// SAFETY: We hold a read lock on the shard, so the entry won't be modified
		// or dropped while the guard exists. The guard ties its lifetime to the lock.
		unsafe { Some(Guard::new(shard, value_ptr)) }
	}

	/// Retrieve a value as `Arc<V>`. Safe to hold across `.await` points.
	///
	/// This is the preferred method for async code.
	pub fn get_arc<K: CacheKey>(&self, key: &K) -> Option<Arc<K::Value>> {
		let erased_key = ErasedKey::new(key);
		let shard_lock = self.get_shard(erased_key.hash);

		// Short-lived read lock
		let arc = {
			let shard = shard_lock.read();
			let entry = shard.get(&erased_key)?;
			entry.value_arc::<K::Value>()?
		}; // Lock released here

		// Increment frequency (lock-free, outside lock)
		self.sketch.increment(erased_key.hash);

		// Mark for promotion if in probationary
		if let Some(shard) = shard_lock.try_read()
			&& let Some(entry) = shard.get(&erased_key)
			&& entry.get_segment() == Segment::Probationary
		{
			self.promotion_buffer.push(erased_key);
		}

		Some(arc)
	}

	/// Retrieve a cloned value. Safe to hold across `.await` points.
	///
	/// Requires `V: Clone`. Use `get_arc()` if clone is expensive.
	pub fn get_clone<K: CacheKey>(&self, key: &K) -> Option<K::Value>
	where
		K::Value: Clone,
	{
		self.get_arc(key).map(|arc| (*arc).clone())
	}

	/// Remove a key from the cache.
	pub fn remove<K: CacheKey>(&self, key: &K) -> Option<Arc<K::Value>> {
		let erased_key = ErasedKey::new(key);
		let shard_lock = self.get_shard(erased_key.hash);

		let mut shard = shard_lock.write();
		let entry = shard.remove(&erased_key)?;

		self.current_size.fetch_sub(entry.size, Ordering::Relaxed);
		self.entry_count.fetch_sub(1, Ordering::Relaxed);

		entry.value_arc::<K::Value>()
	}

	/// Check if a key exists without affecting frequency counters.
	pub fn contains<K: CacheKey>(&self, key: &K) -> bool {
		let erased_key = ErasedKey::new(key);
		let shard_lock = self.get_shard(erased_key.hash);
		let shard = shard_lock.read();
		shard.contains(&erased_key)
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
	pub fn clear(&self) {
		for shard_lock in &self.shards {
			let mut shard = shard_lock.write();
			shard.clear();
		}
		self.current_size.store(0, Ordering::Relaxed);
		self.entry_count.store(0, Ordering::Relaxed);
	}

	/// Get the shard for a given hash.
	fn get_shard(&self, hash: u64) -> &RwLock<Shard> {
		let index = (hash as usize) & (self.shard_count - 1);
		&self.shards[index]
	}

	/// Evict one entry using probabilistic sampling across shards.
	///
	/// Returns true if an entry was evicted.
	fn evict_one(&self) -> bool {
		// Sample one candidate from each shard
		let mut candidates = Vec::with_capacity(self.shard_count);

		for shard_lock in &self.shards {
			if let Some(shard) = shard_lock.try_read()
				&& let Some(candidate) = shard.sample_eviction_candidate(&self.sketch)
			{
				candidates.push(candidate);
			}
		}

		if candidates.is_empty() {
			return false;
		}

		// Find the candidate with the lowest score
		let victim = candidates
			.into_iter()
			.min_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
			.expect("No eviction candidates despite non-empty candidate list");

		// Evict the victim
		let shard_lock = self.get_shard(victim.key.hash);
		let mut shard = shard_lock.write();

		if let Some((_, size)) = shard.evict_one(&self.sketch) {
			self.current_size.fetch_sub(size, Ordering::Relaxed);
			self.entry_count.fetch_sub(1, Ordering::Relaxed);
			true
		} else {
			false
		}
	}

	/// Process pending promotions from the promotion buffer.
	///
	/// This is called periodically during write operations to avoid
	/// acquiring write locks during reads.
	fn drain_promotions(&self) {
		// Limit the number of promotions per call to avoid blocking
		const MAX_PROMOTIONS: usize = 16;

		for _ in 0..MAX_PROMOTIONS {
			if let Some(key) = self.promotion_buffer.pop() {
				let shard_lock = self.get_shard(key.hash);

				// Try to acquire write lock without blocking
				if let Some(mut shard) = shard_lock.try_write() {
					shard.promote(&key);
				} else {
					// Put it back for later
					self.promotion_buffer.push(key);
					break;
				}
			} else {
				break;
			}
		}
	}
}

// Thread safety: Cache can be shared across threads
unsafe impl Send for Cache {}
unsafe impl Sync for Cache {}

#[cfg(test)]
mod tests {
	use deepsize::DeepSizeOf;

	use super::*;

	#[derive(Hash, Eq, PartialEq, Clone, Debug)]
	struct TestKey(u64);

	impl CacheKey for TestKey {
		type Value = TestValue;

		fn weight(&self) -> u64 {
			50
		}
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

		let retrieved = cache.get_arc(&key).unwrap();
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

		let removed = cache.remove(&key).unwrap();
		assert_eq!(*removed, value);
		assert!(!cache.contains(&key));
	}

	#[test]
	fn test_cache_eviction() {
		// Small cache that will trigger eviction
		let cache = Cache::new(200);

		// Insert values that exceed capacity
		for i in 0..10 {
			let key = TestKey(i);
			let value = TestValue {
				data: "x".repeat(50),
			};
			cache.insert(key, value);
		}

		// Cache should have evicted some entries
		assert!(cache.len() < 10);
		assert!(cache.size() <= 200);
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
			handle.join().unwrap();
		}

		assert!(cache.len() > 0);
	}

	#[test]
	fn test_cache_is_send_sync() {
		fn assert_send<T: Send>() {}
		fn assert_sync<T: Sync>() {}

		assert_send::<Cache>();
		assert_sync::<Cache>();
	}
}

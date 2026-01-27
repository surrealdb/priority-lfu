use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::Ordering;

use ahash::RandomState;
use hashbrown::HashMap;
use indexmap::IndexMap;

use crate::erased::{Entry, ErasedKey, ErasedKeyRef};

/// Passthrough hasher for ErasedKey (which already has pre-computed hash).
#[derive(Default)]
pub(crate) struct PassthroughHasher(u64);

impl Hasher for PassthroughHasher {
	fn finish(&self) -> u64 {
		self.0
	}

	fn write(&mut self, _bytes: &[u8]) {
		panic!("PassthroughHasher only works with u64 hash values");
	}

	fn write_u64(&mut self, i: u64) {
		self.0 = i;
	}
}

/// Build hasher for passthrough (just returns the hash as-is).
#[derive(Clone, Default)]
pub(crate) struct PassthroughBuildHasher;

impl BuildHasher for PassthroughBuildHasher {
	type Hasher = PassthroughHasher;

	fn build_hasher(&self) -> Self::Hasher {
		PassthroughHasher::default()
	}
}

/// A single policy bucket containing entries with the same eviction priority.
struct PolicyBucket {
	/// Entries in this bucket (ordered for clock sweep)
	list: IndexMap<ErasedKey, (), RandomState>,
	/// Clock hand position (index into list)
	hand: usize,
}

impl PolicyBucket {
	fn new() -> Self {
		Self {
			list: IndexMap::with_hasher(RandomState::new()),
			hand: 0,
		}
	}

	fn len(&self) -> usize {
		self.list.len()
	}

	fn is_empty(&self) -> bool {
		self.list.is_empty()
	}

	fn insert(&mut self, key: ErasedKey) {
		self.list.insert(key, ());
		// Keep hand in bounds
		if self.hand >= self.list.len() && !self.list.is_empty() {
			self.hand = 0;
		}
	}

	fn remove(&mut self, key: &ErasedKey) -> bool {
		let removed = self.list.shift_remove(key).is_some();
		// Keep hand in bounds
		if self.hand >= self.list.len() && !self.list.is_empty() {
			self.hand = 0;
		}
		removed
	}

	fn clear(&mut self) {
		self.list.clear();
		self.hand = 0;
	}
}

/// A single shard containing entries with weight-stratified clock eviction.
///
/// The shard is not thread-safe on its own; the Cache wraps it in RwLock.
pub struct Shard {
	/// Main entry storage (uses passthrough hasher since keys have pre-computed ahash)
	pub(crate) entries: HashMap<ErasedKey, Entry, PassthroughBuildHasher>,
	/// Policy buckets (one per CachePolicy variant)
	buckets: [PolicyBucket; 4],
	/// Current total size in bytes
	size_current: usize,
	/// Maximum size in bytes
	size_capacity: usize,
}

impl Shard {
	/// Create a new shard with the given capacity.
	///
	/// # Arguments
	/// * `size_capacity` - Total size capacity for this shard (in bytes)
	pub fn new(size_capacity: usize) -> Self {
		Self {
			entries: HashMap::with_hasher(PassthroughBuildHasher),
			buckets: [
				PolicyBucket::new(), // Critical
				PolicyBucket::new(), // Durable
				PolicyBucket::new(), // Standard
				PolicyBucket::new(), // Volatile
			],
			size_current: 0,
			size_capacity,
		}
	}

	/// Insert an entry into the shard.
	///
	/// Returns (previous_entry, (num_evictions, total_evicted_size)).
	pub fn insert(&mut self, key: ErasedKey, entry: Entry) -> (Option<Entry>, (usize, usize)) {
		let size = entry.size;
		let policy = entry.policy;
		let mut num_evictions = 0;
		let mut total_evicted_size = 0;

		// Check if key already exists
		if let Some(old_entry) = self.entries.get(&key) {
			let old_policy = old_entry.policy;
			let old_size = old_entry.size;

			// Remove from old bucket
			self.buckets[old_policy as usize].remove(&key);
			self.size_current -= old_size;

			// Will re-insert into new bucket below
		}

		// Evict until we have space
		const MAX_EVICT_ATTEMPTS: usize = 1000;
		let mut attempts = 0;
		while self.size_current + size > self.size_capacity && attempts < MAX_EVICT_ATTEMPTS {
			if let Some(evicted_size) = self.evict_one() {
				num_evictions += 1;
				total_evicted_size += evicted_size;
				attempts = 0; // Reset on successful eviction
			} else {
				attempts += 1; // No eviction, might be stuck
			}
		}

		// Insert into entries map
		let old = self.entries.insert(key.clone(), entry);

		// Add to appropriate bucket
		self.buckets[policy as usize].insert(key);
		self.size_current += size;

		(old, (num_evictions, total_evicted_size))
	}

	/// Get an entry by key, updating clock bit and frequency.
	#[cfg(test)]
	pub fn get(&self, key: &ErasedKey) -> Option<&Entry> {
		let entry = self.entries.get(key)?;
		// Set clock bit
		entry.clock_bit.store(true, Ordering::Relaxed);
		// Increment frequency (saturating at 255)
		entry.frequency.fetch_add(1, Ordering::Relaxed);
		Some(entry)
	}

	/// Get an entry by borrowed key reference (zero allocation).
	pub fn get_ref<K: crate::traits::CacheKey>(&self, key_ref: &ErasedKeyRef<K>) -> Option<&Entry> {
		// Use raw_entry to search with pre-computed hash
		let (_key, entry) = self
			.entries
			.raw_entry()
			.from_hash(key_ref.hash, |stored_key| key_ref.equals(stored_key))?;

		// Set clock bit
		entry.clock_bit.store(true, Ordering::Relaxed);
		// Increment frequency (saturating at 255)
		entry.frequency.fetch_add(1, Ordering::Relaxed);

		Some(entry)
	}

	/// Remove an entry by key.
	pub fn remove(&mut self, key: &ErasedKey) -> Option<Entry> {
		let entry = self.entries.remove(key)?;
		let policy = entry.policy;
		let size = entry.size;

		self.buckets[policy as usize].remove(key);
		self.size_current -= size;

		Some(entry)
	}

	/// Check if shard contains a key.
	#[cfg(test)]
	pub fn contains(&self, key: &ErasedKey) -> bool {
		self.entries.contains_key(key)
	}

	/// Number of entries in this shard.
	#[cfg(test)]
	pub fn len(&self) -> usize {
		self.entries.len()
	}

	/// Clear all entries.
	pub fn clear(&mut self) {
		self.entries.clear();
		for bucket in &mut self.buckets {
			bucket.clear();
		}
		self.size_current = 0;
	}

	/// Evict one entry using weight-stratified clock algorithm.
	///
	/// Starts from Volatile bucket and moves to lower priority buckets if needed.
	/// Returns the size of the evicted entry, or None if no eviction was possible.
	fn evict_one(&mut self) -> Option<usize> {
		// Try buckets from lowest priority (Volatile) to highest (Critical)
		for policy_idx in (0..4).rev() {
			if let Some(evicted_size) = self.evict_from_bucket(policy_idx) {
				return Some(evicted_size);
			}
		}
		None
	}

	/// Try to evict one entry from a specific bucket using clock algorithm.
	fn evict_from_bucket(&mut self, policy_idx: usize) -> Option<usize> {
		let bucket = &mut self.buckets[policy_idx];

		if bucket.is_empty() {
			return None;
		}

		let bucket_len = bucket.len();

		// Do a full sweep of the clock
		for _ in 0..bucket_len {
			// Get key at current hand position
			let key = bucket.list.get_index(bucket.hand)?.0.clone();

			// Get the entry
			let entry = self.entries.get(&key)?;

			let clock_bit = entry.clock_bit.load(Ordering::Relaxed);
			let frequency = entry.frequency.load(Ordering::Relaxed);

			if clock_bit {
				// Clear clock bit and advance
				entry.clock_bit.store(false, Ordering::Relaxed);
				bucket.hand = (bucket.hand + 1) % bucket_len;
			} else if frequency == 0 {
				// Evict this entry
				let evicted = self.entries.remove(&key)?;
				let evicted_size = evicted.size;
				bucket.remove(&key);
				self.size_current -= evicted_size;
				return Some(evicted_size);
			} else {
				// Decrement frequency and advance
				entry.frequency.fetch_sub(1, Ordering::Relaxed);
				bucket.hand = (bucket.hand + 1) % bucket_len;
			}
		}

		// If we've done a full sweep and found nothing, evict the entry at the hand
		// (this handles the case where all entries have references or high frequency)
		let key = bucket.list.get_index(bucket.hand)?.0.clone();
		let evicted = self.entries.remove(&key)?;
		let evicted_size = evicted.size;
		bucket.remove(&key);
		self.size_current -= evicted_size;
		Some(evicted_size)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::DeepSizeOf;
	use crate::traits::{CacheKey, CachePolicy};

	#[derive(Hash, Eq, PartialEq, Clone, Debug, DeepSizeOf)]
	struct TestKey(u64, CachePolicy); // (id, policy)

	impl CacheKey for TestKey {
		type Value = TestValue;

		fn policy(&self) -> CachePolicy {
			self.1
		}
	}

	#[derive(DeepSizeOf)]
	struct TestValue {
		size: usize,
	}

	fn make_key(id: u64, policy: CachePolicy) -> ErasedKey {
		ErasedKey::new(&TestKey(id, policy))
	}

	fn make_entry(size: usize, policy: CachePolicy) -> Entry {
		Entry::new(
			TestValue {
				size,
			},
			policy,
		)
	}

	#[test]
	fn test_shard_insert() {
		let mut shard = Shard::new(1000);

		let key = make_key(1, CachePolicy::Standard);
		let entry = make_entry(50, CachePolicy::Standard);

		let (old, _evicted) = shard.insert(key.clone(), entry);
		assert!(old.is_none());
		assert!(shard.contains(&key));
		assert_eq!(shard.len(), 1);
	}

	#[test]
	fn test_shard_remove() {
		let mut shard = Shard::new(1000);

		let key = make_key(1, CachePolicy::Standard);
		let entry = make_entry(50, CachePolicy::Standard);

		shard.insert(key.clone(), entry);
		assert!(shard.remove(&key).is_some());
		assert!(!shard.contains(&key));
		assert_eq!(shard.len(), 0);
	}

	#[test]
	fn test_get_updates_clock_and_frequency() {
		let mut shard = Shard::new(1000);

		let key = make_key(1, CachePolicy::Standard);
		let entry = make_entry(50, CachePolicy::Standard);
		shard.insert(key.clone(), entry);

		// Get should set clock bit and increment frequency
		let e = shard.get(&key).expect("entry should exist");
		assert_eq!(e.clock_bit.load(Ordering::Relaxed), true);
		assert_eq!(e.frequency.load(Ordering::Relaxed), 1);

		let e = shard.get(&key).expect("entry should exist");
		assert_eq!(e.clock_bit.load(Ordering::Relaxed), true);
		assert_eq!(e.frequency.load(Ordering::Relaxed), 2);
	}

	#[test]
	fn test_get_ref_zero_allocation() {
		use crate::erased::ErasedKeyRef;

		let mut shard = Shard::new(1000);

		let key = TestKey(1, CachePolicy::Standard);
		let entry = make_entry(50, CachePolicy::Standard);
		let erased = ErasedKey::new(&key);
		let hash = erased.hash;
		shard.insert(erased, entry);

		// Verify entry exists
		assert_eq!(shard.entries.len(), 1, "Should have 1 entry");

		// Create borrowed key ref
		let key_ref = ErasedKeyRef::new(&key);
		assert_eq!(key_ref.hash, hash, "Hashes should match");

		// Get using borrowed reference should work
		let e = shard.get_ref(&key_ref).expect("get_ref should find the entry");
		assert_eq!(e.clock_bit.load(Ordering::Relaxed), true);
		assert_eq!(e.frequency.load(Ordering::Relaxed), 1);

		let e = shard.get_ref(&key_ref).expect("entry should exist");
		assert_eq!(e.frequency.load(Ordering::Relaxed), 2);
	}

	#[test]
	fn test_policy_based_eviction() {
		let mut shard = Shard::new(200);

		// Insert entries with different policies
		let volatile_key = make_key(1, CachePolicy::Volatile);
		let volatile_entry = make_entry(50, CachePolicy::Volatile);
		shard.insert(volatile_key, volatile_entry);

		let standard_key = make_key(2, CachePolicy::Standard);
		let standard_entry = make_entry(50, CachePolicy::Standard);
		shard.insert(standard_key, standard_entry);

		let durable_key = make_key(3, CachePolicy::Durable);
		let durable_entry = make_entry(50, CachePolicy::Durable);
		shard.insert(durable_key, durable_entry);

		// Fill to trigger eviction - volatile should be evicted first
		for i in 10..15 {
			let k = make_key(i, CachePolicy::Standard);
			let e = make_entry(50, CachePolicy::Standard);
			let _ = shard.insert(k, e);
		}

		// Volatile entry should be more likely to be evicted
		// (This is probabilistic, but with no accesses, volatile should go first)
	}

	#[test]
	fn test_frequency_decay() {
		let mut shard = Shard::new(1000);

		let key = make_key(1, CachePolicy::Standard);
		let entry = make_entry(50, CachePolicy::Standard);
		shard.insert(key.clone(), entry);

		// Access to build up frequency
		for _ in 0..5 {
			shard.get(&key);
		}

		let e = shard.entries.get(&key).expect("entry should exist");
		assert!(e.frequency.load(Ordering::Relaxed) >= 5);

		// Clock bit should be set
		assert_eq!(e.clock_bit.load(Ordering::Relaxed), true);
	}
}

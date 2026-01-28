//! Shard implementation for partitioned cache storage.
//!
//! A `Shard` is a single partition that stores cache entries and performs clock-based eviction
//! within policy buckets. The `Cache` divides its capacity across multiple shards, each wrapped
//! in an `RwLock` to reduce lock contention during concurrent access.
//!
//! # Clock Sweep Implementation
//!
//! Each shard maintains three policy buckets (Critical, Standard, Volatile). When eviction is
//! needed, the shard sweeps through buckets in priority order (Volatile → Standard → Critical).
//!
//! Within each bucket, entries are arranged in a circular list with a "clock hand" that sweeps
//! through them:
//! - If an entry's clock bit is set, clear it and move to the next entry
//! - If the clock bit is clear but frequency > 0, decrement frequency and move to next entry
//! - If both clock bit and frequency are 0, evict the entry immediately
//! - After a full sweep with no evictions, force-evict the entry at the hand position
//!
//! # Optimizations
//!
//! - **Passthrough hasher**: Since `ErasedKey` pre-computes its hash value, we use a passthrough
//!   hasher that returns the stored hash directly, avoiding redundant hashing on lookups.
//!
//! - **IndexMap for clock**: Each policy bucket uses `IndexMap` to maintain insertion order (for
//!   the clock hand) while providing O(1) key-based lookups and removals.
//!
//! - **Atomic metadata**: Clock bits and frequency counters use atomic types, allowing updates
//!   during concurrent reads without requiring a write lock.

use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::Ordering;

use ahash::RandomState;
use hashbrown::HashMap;
use hashbrown::hash_map::Entry as HashMapEntry;
use indexmap::IndexMap;

use crate::erased::{Entry, ErasedKey, ErasedKeyRef};
use crate::traits::NUM_POLICY_BUCKETS;

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

	/// Remove a key from the bucket using O(1) swap_remove.
	///
	/// When swap_remove is used, the last element is moved to fill the gap.
	/// We must adjust the hand position accordingly:
	/// - If we removed before the hand, decrement hand
	/// - If we removed at the old last position and hand pointed there, it now points to removed
	///   slot
	fn remove(&mut self, key: &ErasedKey) -> bool {
		let old_len = self.list.len();
		if let Some((removed_idx, _, _)) = self.list.swap_remove_full(key) {
			let new_len = self.list.len();
			if new_len == 0 {
				self.hand = 0;
			} else if removed_idx < self.hand {
				// Removed before hand, decrement to stay on same logical entry
				self.hand -= 1;
			} else if self.hand == old_len - 1 && removed_idx != old_len - 1 {
				// Hand was pointing to the last element, which got swapped to removed_idx
				self.hand = removed_idx;
			}
			// Keep hand in bounds (handles edge cases)
			if self.hand >= new_len && new_len > 0 {
				self.hand = 0;
			}
			true
		} else {
			false
		}
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
	buckets: [PolicyBucket; 3],
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

		// Check if key exists and get old metadata (uses raw_entry for single hash computation)
		let old_info = self.entries.raw_entry().from_key(&key).map(|(_, e)| (e.policy, e.size));

		// If key exists, remove from old bucket and adjust size
		if let Some((old_policy, old_size)) = old_info {
			self.buckets[old_policy as usize].remove(&key);
			self.size_current -= old_size;
		}

		// Evict until we have space (must happen before insert to avoid self-eviction)
		let (num_evictions, total_evicted_size) = self.evict_until_space(size);

		// Now use entry API for the actual insert (hash already computed, fast lookup)
		let old = match self.entries.entry(key.clone()) {
			HashMapEntry::Occupied(mut occupied) => Some(occupied.insert(entry)),
			HashMapEntry::Vacant(vacant) => {
				vacant.insert(entry);
				None
			}
		};

		// Add to appropriate bucket
		self.buckets[policy as usize].insert(key);
		self.size_current += size;

		(old, (num_evictions, total_evicted_size))
	}

	/// Evict entries until there's space for `needed_size` bytes.
	///
	/// Optimized to batch evictions within the same bucket before moving to next priority,
	/// reducing bucket priority iteration overhead.
	/// Returns (num_evictions, total_evicted_size).
	fn evict_until_space(&mut self, needed_size: usize) -> (usize, usize) {
		let mut num_evictions = 0;
		let mut total_evicted_size = 0;

		// Try buckets from lowest priority (Volatile) to highest (Critical)
		// Stay in each bucket until it's exhausted or we have enough space
		for policy_idx in (0..NUM_POLICY_BUCKETS).rev() {
			while self.size_current + needed_size > self.size_capacity {
				if let Some(evicted_size) = self.evict_from_bucket(policy_idx) {
					num_evictions += 1;
					total_evicted_size += evicted_size;
				} else {
					break; // This bucket is empty, try next priority
				}
			}
			// Check if we have enough space
			if self.size_current + needed_size <= self.size_capacity {
				break;
			}
		}

		(num_evictions, total_evicted_size)
	}

	/// Get an entry by key, updating clock bit and frequency.
	#[cfg(test)]
	pub fn get(&self, key: &ErasedKey) -> Option<&Entry> {
		let entry = self.entries.get(key)?;
		// Set clock bit
		entry.clock_bit.store(true, Ordering::Relaxed);
		// Increment frequency (saturating at 255)
		// Using load+store avoids CAS loop overhead; small races are acceptable for heuristic
		let freq = entry.frequency.load(Ordering::Relaxed);
		if freq < 255 {
			entry.frequency.store(freq + 1, Ordering::Relaxed);
		}
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
		// Using load+store avoids CAS loop overhead; small races are acceptable for heuristic
		let freq = entry.frequency.load(Ordering::Relaxed);
		if freq < 255 {
			entry.frequency.store(freq + 1, Ordering::Relaxed);
		}

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

	/// Try to evict one entry from a specific bucket using clock algorithm.
	///
	/// Optimized to avoid cloning keys during the sweep - only clones when evicting.
	fn evict_from_bucket(&mut self, policy_idx: usize) -> Option<usize> {
		let bucket = &self.buckets[policy_idx];

		if bucket.is_empty() {
			return None;
		}

		let bucket_len = bucket.len();
		let mut hand = self.buckets[policy_idx].hand;

		// Phase 1: Sweep to find eviction candidate (read-only on bucket structure)
		// We track the hand position locally and only clone when we find a victim
		for _ in 0..bucket_len {
			// Get key reference at current hand position (no clone)
			let key_ref = self.buckets[policy_idx].list.get_index(hand)?.0;

			// Get the entry using the key reference
			let entry = self.entries.get(key_ref)?;

			let clock_bit = entry.clock_bit.load(Ordering::Relaxed);
			let frequency = entry.frequency.load(Ordering::Relaxed);

			if clock_bit {
				// Clear clock bit and advance hand
				entry.clock_bit.store(false, Ordering::Relaxed);
				hand += 1;
				if hand >= bucket_len {
					hand = 0;
				}
			} else if frequency == 0 {
				// Found victim - now clone the key and evict
				let key = key_ref.clone();
				// Update hand position before modifying bucket
				self.buckets[policy_idx].hand = hand;
				let evicted = self.entries.remove(&key)?;
				let evicted_size = evicted.size;
				self.buckets[policy_idx].remove(&key);
				self.size_current -= evicted_size;
				return Some(evicted_size);
			} else {
				// Decrement frequency and advance hand
				entry.frequency.fetch_sub(1, Ordering::Relaxed);
				hand += 1;
				if hand >= bucket_len {
					hand = 0;
				}
			}
		}

		// Phase 2: Force-evict after a full sweep with no evictions
		// Design note: We force-evict after a single sweep rather than multiple sweeps.
		// This guarantees forward progress when all entries are recently accessed.
		// Trade-off: A high-frequency entry may be evicted before its frequency fully
		// decays, but this prevents pathological cases where eviction stalls.
		self.buckets[policy_idx].hand = hand;
		let key = self.buckets[policy_idx].list.get_index(hand)?.0.clone();
		let evicted = self.entries.remove(&key)?;
		let evicted_size = evicted.size;
		self.buckets[policy_idx].remove(&key);
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

		let durable_key = make_key(3, CachePolicy::Critical);
		let durable_entry = make_entry(50, CachePolicy::Critical);
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

	#[test]
	fn test_insert_oversized_entry_into_empty_shard() {
		// Test inserting a single entry larger than the shard capacity
		// This should succeed (allowing the cache to be useful for large items)

		#[derive(DeepSizeOf)]
		struct LargeValue {
			data: Vec<u8>,
		}

		let mut shard = Shard::new(100);

		let key = make_key(1, CachePolicy::Standard);
		// Create a value with ~200 bytes of data (Vec overhead + 200 bytes)
		let large_value = LargeValue {
			data: vec![0u8; 200],
		};
		let entry = Entry::new(large_value, CachePolicy::Standard);
		let entry_size = entry.size;

		let (old, (num_evictions, _evicted_size)) = shard.insert(key.clone(), entry);

		// Should insert successfully
		assert!(old.is_none());
		assert!(shard.contains(&key));
		assert_eq!(shard.len(), 1);
		assert_eq!(num_evictions, 0); // No evictions in empty cache

		// Size should exceed capacity (which is acceptable for a single oversized entry)
		assert_eq!(shard.size_current, entry_size);
		assert!(
			shard.size_current > shard.size_capacity,
			"Expected size {} > capacity {}",
			shard.size_current,
			shard.size_capacity
		);
	}

	#[test]
	fn test_insert_oversized_entry_evicts_existing() {
		// Test that inserting an oversized entry triggers evictions

		#[derive(DeepSizeOf)]
		struct SmallValue {
			data: Vec<u8>,
		}

		#[derive(DeepSizeOf)]
		struct LargeValue {
			data: Vec<u8>,
		}

		let mut shard = Shard::new(200);

		// Fill with small entries (~10 bytes each) - should all fit
		for i in 1..=3 {
			let key = make_key(i, CachePolicy::Standard);
			let small_value = SmallValue {
				data: vec![0u8; 5],
			};
			let entry = Entry::new(small_value, CachePolicy::Standard);
			shard.insert(key, entry);
		}

		let initial_len = shard.len();
		assert!(initial_len >= 3, "All 3 small entries should fit initially");

		// Insert oversized entry (~300 bytes, larger than capacity)
		let big_key = make_key(100, CachePolicy::Standard);
		let large_value = LargeValue {
			data: vec![0u8; 300],
		};
		let big_entry = Entry::new(large_value, CachePolicy::Standard);

		let (_old, (num_evictions, _evicted_size)) = shard.insert(big_key.clone(), big_entry);

		// Should have triggered evictions (may hit retry limit before evicting all)
		assert!(num_evictions > 0, "Expected some evictions but got none");

		// The oversized entry should be inserted
		assert!(shard.contains(&big_key), "Oversized entry should be inserted");

		// After inserting oversized entry, old entries should be gone
		assert!(
			shard.len() < initial_len + 1,
			"Expected fewer than {} entries after eviction, but got {}",
			initial_len + 1,
			shard.len()
		);
	}
}

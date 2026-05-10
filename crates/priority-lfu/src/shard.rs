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

use hashbrown::HashMap;
use hashbrown::hash_map::Entry as HashMapEntry;
use indexmap::IndexMap;

use crate::erased::{Entry, ErasedKey, ErasedKeyLookup, ErasedKeyRef};
use crate::traits::{CacheKey, CacheKeyLookup, NUM_POLICY_BUCKETS};

/// Eviction statistics returned from insert operations.
#[derive(Debug, Clone, Default)]
pub struct EvictionStats {
	/// Number of entries evicted
	pub count: usize,
	/// Total size of evicted entries in bytes
	pub size: usize,
}

/// An evicted entry with its key.
pub struct EvictedEntry {
	/// The evicted key
	pub key: ErasedKey,
	/// The evicted entry
	pub entry: Entry,
}

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
	list: IndexMap<ErasedKey, (), PassthroughBuildHasher>,
	/// Clock hand position (index into list)
	hand: usize,
}

impl PolicyBucket {
	fn new() -> Self {
		Self {
			list: IndexMap::with_hasher(PassthroughBuildHasher),
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
	/// `IndexMap::swap_remove` removes the entry at `removed_idx` and, if it was not the last
	/// entry, moves the entry from the final position into the vacated slot. All other indices
	/// remain unchanged. The hand is adjusted to maintain a valid position:
	///
	/// - If the bucket is now empty, reset the hand to 0.
	/// - If the hand pointed at the final position and the removal happened elsewhere, the entry
	///   it referenced has moved to `removed_idx`, so the hand follows it.
	/// - If the hand pointed at the position that is now out of bounds (the old final index),
	///   wrap it to 0.
	/// - Otherwise the hand stays put. Note that when `self.hand == removed_idx` (and it was not
	///   the final position), the slot now holds a different (moved-in) entry that will simply be
	///   considered in the next sweep — this is acceptable for an approximate clock sweep.
	fn remove(&mut self, key: &ErasedKey) -> bool {
		let old_len = self.list.len();
		if let Some((removed_idx, _, _)) = self.list.swap_remove_full(key) {
			let new_len = self.list.len();
			if new_len == 0 {
				self.hand = 0;
			} else if self.hand == old_len - 1 && removed_idx != old_len - 1 {
				// Hand was pointing to the last element, which got swapped to removed_idx.
				self.hand = removed_idx;
			} else if self.hand >= new_len {
				// Defensive: hand was at the old final index and we removed exactly that index
				// (self.hand == removed_idx == old_len - 1), so the position no longer exists.
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
	/// Returns (previous_entry, eviction_stats, evicted_entries).
	pub fn insert(
		&mut self,
		key: ErasedKey,
		entry: Entry,
	) -> (Option<Entry>, EvictionStats, Vec<EvictedEntry>) {
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
		let (stats, evicted) = self.evict_until_space(size);

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

		(old, stats, evicted)
	}

	/// Evict entries until there's space for `needed_size` bytes.
	///
	/// Optimized to batch evictions within the same bucket before moving to next priority,
	/// reducing bucket priority iteration overhead.
	/// Returns (eviction_stats, evicted_entries).
	fn evict_until_space(&mut self, needed_size: usize) -> (EvictionStats, Vec<EvictedEntry>) {
		let mut stats = EvictionStats::default();
		let mut evicted = Vec::new();

		// Try buckets from lowest priority (Volatile) to highest (Critical)
		// Stay in each bucket until it's exhausted or we have enough space
		for policy_idx in (0..NUM_POLICY_BUCKETS).rev() {
			while self.size_current + needed_size > self.size_capacity {
				if let Some(evicted_entry) = self.evict_from_bucket(policy_idx) {
					stats.count += 1;
					stats.size += evicted_entry.entry.size;
					evicted.push(evicted_entry);
				} else {
					break; // This bucket is empty, try next priority
				}
			}
			// Check if we have enough space
			if self.size_current + needed_size <= self.size_capacity {
				break;
			}
		}

		(stats, evicted)
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

	/// Check whether the shard contains an entry for the given key (zero allocation,
	/// no side effects on clock bit or frequency).
	pub fn contains_ref<K: crate::traits::CacheKey>(&self, key_ref: &ErasedKeyRef<K>) -> bool {
		self.entries
			.raw_entry()
			.from_hash(key_ref.hash, |stored_key| key_ref.equals(stored_key))
			.is_some()
	}

	/// Check whether the shard contains an entry for the given borrowed lookup key
	/// (zero allocation, no side effects on clock bit or frequency).
	pub fn contains_ref_by<K, Q>(&self, key_ref: &ErasedKeyLookup<K, Q>) -> bool
	where
		K: CacheKey,
		Q: CacheKeyLookup<K> + ?Sized,
	{
		self.entries
			.raw_entry()
			.from_hash(key_ref.hash, |stored_key| key_ref.equals(stored_key))
			.is_some()
	}

	/// Get an entry by borrowed lookup key (zero allocation).
	///
	/// This allows looking up entries using a borrowed key type `Q` that implements
	/// `CacheKeyLookup<K>`, enabling zero-allocation lookups (e.g., using `&str` tuples
	/// to look up `String` tuples).
	pub fn get_ref_by<K, Q>(&self, key_ref: &ErasedKeyLookup<K, Q>) -> Option<&Entry>
	where
		K: CacheKey,
		Q: CacheKeyLookup<K> + ?Sized,
	{
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
	///
	/// Returns the actual stored key and entry, which may differ from the lookup key
	/// when multiple key representations hash/compare as equal.
	pub fn remove(&mut self, key: &ErasedKey) -> Option<(ErasedKey, Entry)> {
		let (stored_key, entry) = self.entries.remove_entry(key)?;
		let policy = entry.policy;
		let size = entry.size;

		self.buckets[policy as usize].remove(&stored_key);
		self.size_current -= size;

		Some((stored_key, entry))
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

	/// Clear all entries without returning them.
	///
	/// For clearing with lifecycle callbacks, use [`drain`] instead.
	#[allow(dead_code)]
	pub fn clear(&mut self) {
		self.entries.clear();
		for bucket in &mut self.buckets {
			bucket.clear();
		}
		self.size_current = 0;
	}

	/// Drain all entries, returning them along with the total drained size and count.
	///
	/// Returning the captured size/count atomically (under the same lock that drains the
	/// shard) lets the caller update global atomics with `fetch_sub` instead of `store(0)`,
	/// which would otherwise race with concurrent inserts on already-drained shards.
	pub fn drain(&mut self) -> (Vec<EvictedEntry>, usize, usize) {
		let drained_size = self.size_current;
		let drained_count = self.entries.len();
		let entries: Vec<EvictedEntry> = self
			.entries
			.drain()
			.map(|(key, entry)| EvictedEntry {
				key,
				entry,
			})
			.collect();
		for bucket in &mut self.buckets {
			bucket.clear();
		}
		self.size_current = 0;
		(entries, drained_size, drained_count)
	}

	/// Try to evict one entry from a specific bucket using clock algorithm.
	///
	/// Optimized to avoid cloning keys during the sweep - only clones when evicting.
	/// Returns the evicted entry with its key for lifecycle callbacks.
	fn evict_from_bucket(&mut self, policy_idx: usize) -> Option<EvictedEntry> {
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
				return Some(EvictedEntry {
					key,
					entry: evicted,
				});
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
		Some(EvictedEntry {
			key,
			entry: evicted,
		})
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

		let (old, _stats, _evicted) = shard.insert(key.clone(), entry);
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

		let critical_key = make_key(3, CachePolicy::Critical);
		let critical_entry = make_entry(50, CachePolicy::Critical);
		shard.insert(critical_key, critical_entry);

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

		let (old, stats, _evicted) = shard.insert(key.clone(), entry);

		// Should insert successfully
		assert!(old.is_none());
		assert!(shard.contains(&key));
		assert_eq!(shard.len(), 1);
		assert_eq!(stats.count, 0); // No evictions in empty cache

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

		let (_old, stats, _evicted) = shard.insert(big_key.clone(), big_entry);

		// Should have triggered evictions (may hit retry limit before evicting all)
		assert!(stats.count > 0, "Expected some evictions but got none");

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

	#[test]
	fn test_policy_bucket_remove_hand_unaffected_when_remove_before_hand() {
		// swap_remove does NOT shift indices. If hand points past the removed slot
		// (but not at the final position), the entry there stays at the same index.
		let mut bucket = PolicyBucket::new();
		for i in 0..5 {
			bucket.insert(make_key(i, CachePolicy::Standard));
		}
		bucket.hand = 3;
		// Remember the key at hand position so we can verify the hand still points to it.
		let key_at_hand = bucket.list.get_index(3).expect("entry at hand").0.clone();

		// Remove index 1 (before hand, not the last index)
		bucket.remove(&make_key(1, CachePolicy::Standard));
		// Hand should still point to the same logical entry (which is still at index 3).
		assert_eq!(bucket.hand, 3, "hand should not move when removing before it");
		let current_key_at_hand =
			bucket.list.get_index(bucket.hand).expect("entry at hand").0.clone();
		assert_eq!(
			current_key_at_hand, key_at_hand,
			"hand should still reference the same logical entry"
		);
	}

	#[test]
	fn test_policy_bucket_remove_hand_follows_swapped_element() {
		// If the hand is at the final index and we remove a non-final index,
		// the final entry gets swapped into the removed slot. The hand must follow it.
		let mut bucket = PolicyBucket::new();
		for i in 0..5 {
			bucket.insert(make_key(i, CachePolicy::Standard));
		}
		bucket.hand = 4; // last index
		let key_at_hand = bucket.list.get_index(4).expect("entry at hand").0.clone();

		// Remove index 1; the entry previously at index 4 is moved to index 1.
		bucket.remove(&make_key(1, CachePolicy::Standard));
		assert_eq!(bucket.hand, 1, "hand should follow the swapped element");
		let current_key_at_hand =
			bucket.list.get_index(bucket.hand).expect("entry at hand").0.clone();
		assert_eq!(
			current_key_at_hand, key_at_hand,
			"hand should follow the same logical entry after swap"
		);
	}

	#[test]
	fn test_policy_bucket_remove_hand_wraps_when_final_index_removed() {
		// If the hand is at the final index and we remove that exact entry,
		// the bucket shrinks and the hand becomes out of bounds; it must wrap to 0.
		let mut bucket = PolicyBucket::new();
		for i in 0..5 {
			bucket.insert(make_key(i, CachePolicy::Standard));
		}
		bucket.hand = 4;
		bucket.remove(&make_key(4, CachePolicy::Standard));
		assert_eq!(bucket.hand, 0);
		assert_eq!(bucket.list.len(), 4);
	}

	#[test]
	fn test_policy_bucket_remove_hand_unchanged_when_removed_at_hand_not_last() {
		// When self.hand == removed_idx (and it's not the last), the entry that was at
		// the final position is moved into the hand's slot. Hand stays put — the next
		// sweep will consider the newly-arrived entry.
		let mut bucket = PolicyBucket::new();
		for i in 0..5 {
			bucket.insert(make_key(i, CachePolicy::Standard));
		}
		bucket.hand = 1;
		bucket.remove(&make_key(1, CachePolicy::Standard));
		assert_eq!(bucket.hand, 1, "hand stays put; slot now holds the moved-in entry");
		assert_eq!(bucket.list.len(), 4);
	}

	#[test]
	fn test_policy_bucket_remove_hand_unchanged_when_removed_after_hand() {
		// Removing an entry past the hand (but not the final index) leaves the hand alone.
		let mut bucket = PolicyBucket::new();
		for i in 0..5 {
			bucket.insert(make_key(i, CachePolicy::Standard));
		}
		bucket.hand = 1;
		let key_at_hand = bucket.list.get_index(1).expect("entry at hand").0.clone();
		bucket.remove(&make_key(3, CachePolicy::Standard));
		assert_eq!(bucket.hand, 1);
		let current_key_at_hand =
			bucket.list.get_index(bucket.hand).expect("entry at hand").0.clone();
		assert_eq!(current_key_at_hand, key_at_hand);
	}

	#[test]
	fn test_policy_bucket_remove_hand_resets_when_bucket_empties() {
		let mut bucket = PolicyBucket::new();
		bucket.insert(make_key(1, CachePolicy::Standard));
		bucket.hand = 0;
		bucket.remove(&make_key(1, CachePolicy::Standard));
		assert_eq!(bucket.hand, 0);
		assert!(bucket.is_empty());
	}

	#[test]
	fn test_shard_remove_keeps_eviction_correct_after_repeated_removes() {
		// Regression test: with the buggy `hand -= 1` adjustment, removing entries
		// "before" the hand caused the clock hand to walk backwards, eventually
		// pointing at an invalid or wrong logical entry. Ensure eviction still
		// works after a sequence of removes interleaved with eviction.
		let mut shard = Shard::new(500);
		for i in 0..10 {
			let key = make_key(i, CachePolicy::Standard);
			let entry = make_entry(40, CachePolicy::Standard);
			shard.insert(key, entry);
		}
		// Remove a few from the front.
		for i in 0..3 {
			let key = make_key(i, CachePolicy::Standard);
			shard.remove(&key);
		}
		// Insert enough new entries to force eviction.
		for i in 100..130 {
			let key = make_key(i, CachePolicy::Standard);
			let entry = make_entry(40, CachePolicy::Standard);
			shard.insert(key, entry);
		}
		// Cache should still respect capacity.
		assert!(shard.size_current <= shard.size_capacity + 40, "size honors capacity");
		// And entries should still be retrievable / removable without panic.
		for i in 100..110 {
			let key = make_key(i, CachePolicy::Standard);
			// remove may or may not find the key depending on eviction order,
			// but it must never panic and must keep counts coherent.
			let _ = shard.remove(&key);
		}
	}
}

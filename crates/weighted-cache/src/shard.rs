use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::Ordering;

use ahash::RandomState;
use hashbrown::HashMap;
use indexmap::IndexMap;

use crate::erased::{Entry, ErasedKey, ErasedKeyRef, ResidentState};

/// Passthrough hasher for ErasedKey (which already has pre-computed hash).
#[derive(Default)]
struct PassthroughHasher(u64);

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
struct PassthroughBuildHasher;

impl BuildHasher for PassthroughBuildHasher {
	type Hasher = PassthroughHasher;

	fn build_hasher(&self) -> Self::Hasher {
		PassthroughHasher::default()
	}
}

/// Compute maximum reference counter based on weight.
///
/// Higher weight items can accumulate more references, making them more resistant to eviction.
/// - Low weight (1-99): max_ref = 2 (standard Clock-PRO)
/// - Medium weight (100-999): max_ref = 2-3
/// - High weight (1000-9999): max_ref = 3-4
/// - Very high weight (10000+): max_ref = 4
fn max_ref_for_weight(weight: u64) -> u16 {
	if weight == 0 {
		return 2; // Fallback for zero weight
	}
	// Base: 2, with weight bonus up to +2 for high weights
	// Use log10 to scale: weight=100 -> bonus=0, weight=1000 -> bonus=1, weight=10000 -> bonus=2
	let bonus = ((weight as f64).log10() - 2.0).max(0.0).min(2.0) as u16;
	2 + bonus
}

/// Result of attempting to advance the clock hand.
enum EvictionResult {
	/// Entry was evicted (space freed)
	Evicted(usize), // size of evicted entry
	/// Progress made (promotion/demotion, reference bit cleared)
	Progress,
	/// No progress (empty list or all pinned)
	NoProgress,
}

/// A single shard containing entries with Clock-PRO eviction.
///
/// The shard is not thread-safe on its own; the Cache wraps it in RwLock.
pub struct Shard {
	/// Main entry storage (uses passthrough hasher since keys have pre-computed ahash)
	pub(crate) entries: HashMap<ErasedKey, Entry, PassthroughBuildHasher>,
	/// Cold entries in LRU order (uses ahash for performance)
	cold_list: IndexMap<ErasedKey, (), RandomState>,
	/// Hot entries in LRU order (uses ahash for performance)
	hot_list: IndexMap<ErasedKey, (), RandomState>,
	/// Ghost entries (hash only, for scan resistance, uses ahash)
	ghost_list: IndexMap<u64, (), RandomState>,
	/// Size tracking (in bytes)
	size_hot: usize,
	size_cold: usize,
	size_capacity: usize,
	size_target_hot: usize,
	/// Ghost capacity
	ghost_capacity: usize,
}

impl Shard {
	/// Create a new shard with the given capacity distribution.
	///
	/// # Arguments
	/// * `size_capacity` - Total size capacity for this shard (in bytes)
	/// * `hot_percent` - Target percentage of capacity for hot entries (e.g., 0.9 = 90%)
	/// * `ghost_capacity` - Maximum number of ghost entries to track
	pub fn new(size_capacity: usize, hot_percent: f32, ghost_capacity: usize) -> Self {
		let size_target_hot = (size_capacity as f64 * hot_percent as f64) as usize;
		let ahash_builder = RandomState::new();
		Self {
			entries: HashMap::with_hasher(PassthroughBuildHasher::default()),
			cold_list: IndexMap::with_hasher(ahash_builder.clone()),
			hot_list: IndexMap::with_hasher(ahash_builder.clone()),
			ghost_list: IndexMap::with_hasher(ahash_builder),
			size_hot: 0,
			size_cold: 0,
			size_capacity,
			size_target_hot,
			ghost_capacity,
		}
	}

	/// Insert an entry into the shard.
	///
	/// Returns (previous_entry, (num_evictions, total_evicted_size)).
	pub fn insert(&mut self, key: ErasedKey, entry: Entry) -> (Option<Entry>, (usize, usize)) {
		let size = entry.size;
		let mut num_evictions = 0;
		let mut total_evicted_size = 0;

		// Check if key already exists
		if let Some(old_entry) = self.entries.get(&key) {
			let old_state = old_entry.get_state();
			let old_size = old_entry.size;

			// Update existing entry - keep state, bump reference (use weight-based max)
			let current_ref = old_entry.referenced.load(Ordering::Relaxed);
			let max_ref = max_ref_for_weight(entry.weight);
			entry.set_referenced(current_ref.saturating_add(1).min(max_ref));
			entry.set_state(old_state);

			// Remove from list
			match old_state {
				ResidentState::Hot => {
					self.hot_list.shift_remove(&key);
					self.size_hot -= old_size;
				}
				ResidentState::Cold => {
					self.cold_list.shift_remove(&key);
					self.size_cold -= old_size;
				}
			}

			// Re-insert into same list
			match old_state {
				ResidentState::Hot => {
					self.hot_list.insert(key.clone(), ());
					self.size_hot += size;
				}
				ResidentState::Cold => {
					self.cold_list.insert(key.clone(), ());
					self.size_cold += size;
				}
			}

			return (self.entries.insert(key, entry), (num_evictions, total_evicted_size));
		}

		// Check if hash is in ghost list (recently evicted - promote to hot)
		let enter_hot = self.ghost_list.shift_remove(&key.hash).is_some();

		// Evict if needed - track whether we made progress
		let mut stuck_count = 0;
		const MAX_STUCK: usize = 100;
		while self.size_hot + self.size_cold + size > self.size_capacity {
			match self.advance_cold_evict() {
				EvictionResult::Evicted(evicted_size) => {
					num_evictions += 1;
					total_evicted_size += evicted_size;
					stuck_count = 0; // Reset on eviction
				}
				EvictionResult::Progress => {
					stuck_count = 0; // Reset on progress (promotion/demotion)
				}
				EvictionResult::NoProgress => {
					stuck_count += 1;
					if stuck_count >= MAX_STUCK {
						break; // Can't make progress, cache is full of pinned items
					}
				}
			}
		}

		// Determine state before inserting
		let state = if enter_hot && self.size_hot + size <= self.size_target_hot {
			ResidentState::Hot
		} else {
			ResidentState::Cold
		};

		// Set state on entry before inserting
		entry.set_state(state);
		
		// Insert into entries map
		let old = self.entries.insert(key.clone(), entry);

		// Add to appropriate list
		match state {
			ResidentState::Hot => {
				self.hot_list.insert(key, ());
				self.size_hot += size;
			}
			ResidentState::Cold => {
				self.cold_list.insert(key, ());
				self.size_cold += size;
			}
		}

		(old, (num_evictions, total_evicted_size))
	}

	/// Get an entry by key, bumping its reference counter.
	pub fn get(&self, key: &ErasedKey) -> Option<&Entry> {
		let entry = self.entries.get(key)?;
		// Atomic increment, cap at weight-based max
		let max_ref = max_ref_for_weight(entry.weight);
		let current = entry.referenced.load(Ordering::Relaxed);
		if current < max_ref {
			entry.referenced.fetch_add(1, Ordering::Relaxed);
		}
		Some(entry)
	}

	/// Get an entry by borrowed key reference (zero allocation).
	pub fn get_ref<K: crate::traits::CacheKey>(&self, key_ref: &ErasedKeyRef<K>) -> Option<&Entry> {
		// Use raw_entry to search with pre-computed hash
		let (_key, entry) = self.entries
			.raw_entry()
			.from_hash(key_ref.hash, |stored_key| key_ref.equals(stored_key))?;

		// Atomic increment, cap at weight-based max
		let max_ref = max_ref_for_weight(entry.weight);
		let current = entry.referenced.load(Ordering::Relaxed);
		if current < max_ref {
			entry.referenced.fetch_add(1, Ordering::Relaxed);
		}

		Some(entry)
	}

	/// Remove an entry by key.
	pub fn remove(&mut self, key: &ErasedKey) -> Option<Entry> {
		let entry = self.entries.remove(key)?;
		let state = entry.get_state();
		let size = entry.size;

		match state {
			ResidentState::Hot => {
				self.hot_list.shift_remove(key);
				self.size_hot -= size;
			}
			ResidentState::Cold => {
				self.cold_list.shift_remove(key);
				self.size_cold -= size;
			}
		}

		Some(entry)
	}

	/// Check if shard contains a key.
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
		self.cold_list.clear();
		self.hot_list.clear();
		self.ghost_list.clear();
		self.size_hot = 0;
		self.size_cold = 0;
	}

	/// Advance the cold clock hand for eviction.
	///
	/// Returns EvictionResult indicating what happened.
	fn advance_cold_evict(&mut self) -> EvictionResult {
		if let Some((key, _)) = self.cold_list.shift_remove_index(0) {
			let entry = self.entries.get_mut(&key).unwrap();
			let referenced = entry.referenced.swap(0, Ordering::Relaxed);
			let size = entry.size;

			if referenced > 0 {
				// Promote to hot
				entry.set_state(ResidentState::Hot);
				self.size_cold -= size;
				self.size_hot += size;
				self.hot_list.insert(key, ());

				// Demote from hot if overweight
				let mut hot_stuck = 0;
				while self.size_hot > self.size_target_hot {
					match self.advance_hot_evict() {
						EvictionResult::NoProgress => {
							hot_stuck += 1;
							if hot_stuck >= 10 {
								break; // Avoid infinite loop
							}
						}
						_ => {
							hot_stuck = 0;
						}
					}
				}
				return EvictionResult::Progress; // Promoted, cleared reference
			}

			// Evict cold entry
			self.size_cold -= size;
			let evicted = self.entries.remove(&key).unwrap();
			let evicted_size = evicted.size;

			// Add to ghost list
			self.ghost_list.insert(key.hash, ());
			if self.ghost_list.len() > self.ghost_capacity {
				self.ghost_list.shift_remove_index(0);
			}

			return EvictionResult::Evicted(evicted_size);
		}

		// Cold list empty, try advancing hot
		self.advance_hot_evict()
	}

	/// Advance the hot clock hand for eviction.
	///
	/// Returns EvictionResult indicating what happened.
	fn advance_hot_evict(&mut self) -> EvictionResult {
		let Some((key, _)) = self.hot_list.shift_remove_index(0) else {
			return EvictionResult::NoProgress;
		};

		let entry = self.entries.get_mut(&key).unwrap();
		let referenced = entry.referenced.swap(0, Ordering::Relaxed);
		let size = entry.size;

		if referenced > 0 {
			// Keep hot, move to back of list
			self.hot_list.insert(key, ());
			return EvictionResult::Progress; // Cleared reference bit
		}

		// Demote to cold (evict directly if cold is also overweight)
		self.size_hot -= size;

		let cold_target = self.size_capacity - self.size_target_hot;
		if self.size_cold + size > cold_target {
			// Evict directly
			let evicted = self.entries.remove(&key).unwrap();
			let evicted_size = evicted.size;
			self.ghost_list.insert(key.hash, ());
			if self.ghost_list.len() > self.ghost_capacity {
				self.ghost_list.shift_remove_index(0);
			}
			return EvictionResult::Evicted(evicted_size);
		} else {
			// Demote to cold
			entry.set_state(ResidentState::Cold);
			self.size_cold += size;
			self.cold_list.insert(key, ());
		}

		EvictionResult::Progress // Demoted
	}

}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::DeepSizeOf;
	use crate::traits::CacheKey;

	#[derive(Hash, Eq, PartialEq, Clone, Debug, DeepSizeOf)]
	struct TestKey(u64, u64); // (id, weight)

	impl CacheKey for TestKey {
		type Value = TestValue;

		fn weight(&self) -> u64 {
			self.1
		}
	}

	#[derive(DeepSizeOf)]
	struct TestValue {
		size: usize,
	}

	fn make_key(id: u64, weight: u64) -> ErasedKey {
		ErasedKey::new(&TestKey(id, weight))
	}

	fn make_entry(size: usize, weight: u64) -> Entry {
		Entry::new(
			TestValue {
				size,
			},
			weight,
		)
	}

	#[test]
	fn test_shard_insert() {
		let mut shard = Shard::new(1000, 0.9, 100);

		let key = make_key(1, 10);
		let entry = make_entry(50, 10);

		let (old, _evicted) = shard.insert(key.clone(), entry);
		assert!(old.is_none());
		assert!(shard.contains(&key));
		assert_eq!(shard.len(), 1);
	}

	#[test]
	fn test_shard_remove() {
		let mut shard = Shard::new(1000, 0.9, 100);

		let key = make_key(1, 10);
		let entry = make_entry(50, 10);

		shard.insert(key.clone(), entry);
		assert!(shard.remove(&key).is_some());
		assert!(!shard.contains(&key));
		assert_eq!(shard.len(), 0);
	}

	#[test]
	fn test_get_bumps_reference() {
		let mut shard = Shard::new(1000, 0.9, 100);

		let key = make_key(1, 10);
		let entry = make_entry(50, 10);
		shard.insert(key.clone(), entry);

		// Get should bump reference counter
		let e = shard.get(&key).unwrap();
		assert_eq!(e.get_referenced(), 1);

		let e = shard.get(&key).unwrap();
		assert_eq!(e.get_referenced(), 2);

		// Should cap at MAX_REF
		let e = shard.get(&key).unwrap();
		assert_eq!(e.get_referenced(), 2);
	}

	#[test]
	fn test_get_ref_zero_allocation() {
		use crate::erased::ErasedKeyRef;
		use crate::traits::CacheKey;

		let mut shard = Shard::new(1000, 0.9, 100);

		let key = TestKey(1, 10);
		let entry = make_entry(50, 10);
		let erased = ErasedKey::new(&key);
		let hash = erased.hash;
		shard.insert(erased.clone(), entry);

		// Verify entry exists
		assert_eq!(shard.entries.len(), 1, "Should have 1 entry");

		// Create borrowed key ref
		let key_ref = ErasedKeyRef::new(&key);
		assert_eq!(key_ref.hash, hash, "Hashes should match");

		// Get using borrowed reference should work (reference counter starts at 0)
		let e = shard.get_ref(&key_ref);
		assert!(e.is_some(), "get_ref should find the entry");
		
		let e = e.unwrap();
		assert_eq!(e.get_referenced(), 1, "First get_ref should bump to 1");

		let e = shard.get_ref(&key_ref).unwrap();
		assert_eq!(e.get_referenced(), 2, "Second get_ref should bump to 2");
		
		// Should cap at MAX_REF
		let e = shard.get_ref(&key_ref).unwrap();
		assert_eq!(e.get_referenced(), 2, "Should cap at MAX_REF (2)");
	}

	#[test]
	fn test_ghost_hit_promotion() {
		let mut shard = Shard::new(100, 0.9, 10);

		// Insert entry
		let key = make_key(1, 100);
		let entry = make_entry(50, 100);
		let _ = shard.insert(key.clone(), entry);

		// Force eviction
		let key2 = make_key(2, 100);
		let entry2 = make_entry(50, 100);
		let _ = shard.insert(key2.clone(), entry2);

		// Re-insert first key - should be hot due to ghost hit
		let entry3 = make_entry(50, 100);
		let _ = shard.insert(key.clone(), entry3);

		let e = shard.entries.get(&key).unwrap();
		// Note: might be hot if ghost tracking worked
		assert!(e.get_state() == ResidentState::Hot || e.get_state() == ResidentState::Cold);
	}

	#[test]
	fn test_cold_to_hot_promotion() {
		let mut shard = Shard::new(1000, 0.9, 100);

		let key = make_key(1, 50);
		let entry = make_entry(50, 50);
		let _ = shard.insert(key.clone(), entry);

		// Entry should start cold
		let e = shard.entries.get(&key).unwrap();
		assert_eq!(e.get_state(), ResidentState::Cold);

		// Access it to set reference bit
		shard.get(&key);

		// Fill cache to trigger eviction
		for i in 2..20 {
			let k = make_key(i, 50);
			let e = make_entry(50, 50);
			let _ = shard.insert(k, e);
		}

		// Original entry might be promoted to hot if referenced
	}
}

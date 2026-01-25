use std::collections::HashMap;

use crate::erased::{Entry, ErasedKey, Segment};
use crate::segments::{SegmentedLRU, Window};
use crate::sketch::FrequencySketch;

/// A single shard containing entries and segment structures.
///
/// The shard is not thread-safe on its own; the Cache wraps it in RwLock.
pub struct Shard {
	/// Main entry storage (all segments share this)
	entries: HashMap<ErasedKey, Entry>,
	/// Window segment (~1% of capacity)
	window: Window,
	/// Main cache segments (probationary + protected)
	slru: SegmentedLRU,
}

impl Shard {
	/// Create a new shard with the given capacity distribution.
	pub fn new(window_size: usize, probationary_size: usize, protected_size: usize) -> Self {
		Self {
			entries: HashMap::new(),
			window: Window::new(window_size),
			slru: SegmentedLRU::new(probationary_size, protected_size),
		}
	}

	/// Insert an entry into the shard.
	///
	/// Returns the previous entry if the key already existed.
	pub fn insert(&mut self, key: ErasedKey, entry: Entry) -> Option<Entry> {
		// Check if key already exists
		if let Some(old_entry) = self.entries.get(&key) {
			let old_segment = old_entry.get_segment();
			let old_size = old_entry.size;

			// Remove from old segment
			match old_segment {
				Segment::Window => {
					self.window.remove(&key, old_size);
				}
				Segment::Probationary => {
					self.slru.remove_probationary(&key, old_size);
				}
				Segment::Protected => {
					self.slru.remove_protected(&key, old_size);
				}
			}
		}

		// Insert into window
		self.window.insert(key.clone(), entry.size);
		entry.set_segment(Segment::Window);

		self.entries.insert(key, entry)
	}

	/// Get an entry by key.
	pub fn get(&self, key: &ErasedKey) -> Option<&Entry> {
		self.entries.get(key)
	}

	/// Remove an entry by key.
	pub fn remove(&mut self, key: &ErasedKey) -> Option<Entry> {
		if let Some(entry) = self.entries.remove(key) {
			let segment = entry.get_segment();
			match segment {
				Segment::Window => {
					self.window.remove(key, entry.size);
				}
				Segment::Probationary => {
					self.slru.remove_probationary(key, entry.size);
				}
				Segment::Protected => {
					self.slru.remove_protected(key, entry.size);
				}
			}
			Some(entry)
		} else {
			None
		}
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
		self.window.clear();
		self.slru.clear();
	}

	/// Promote an entry from probationary to protected.
	pub fn promote(&mut self, key: &ErasedKey) -> bool {
		if let Some(entry) = self.entries.get(key)
			&& entry.get_segment() == Segment::Probationary
			&& self.slru.promote(key, entry.size)
		{
			entry.set_segment(Segment::Protected);
			return true;
		}
		false
	}

	/// Admit entries from window to main cache.
	///
	/// Returns the number of evictions performed.
	pub fn admit_from_window(&mut self, sketch: &FrequencySketch) -> usize {
		let mut evictions = 0;

		while self.window.is_full() {
			if let Some(candidate_key) = self.window.pop_front() {
				// Get candidate entry size and weight (need to extract before borrowing mutably)
				let (candidate_size, candidate_weight, candidate_hash) =
					match self.entries.get(&candidate_key) {
						Some(e) => (e.size, e.weight, candidate_key.hash),
						None => continue,
					};

				// Update window size since we popped the entry
				self.window.size = self.window.size.saturating_sub(candidate_size);

				// Check if we have space in probationary
				if !self.slru.probationary_is_full() {
					// Direct admission
					self.slru.insert_probationary(candidate_key.clone(), candidate_size);
					if let Some(candidate) = self.entries.get(&candidate_key) {
						candidate.set_segment(Segment::Probationary);
					}
				} else {
					// Need to evict from probationary
					if let Some(victim_key) = self.slru.peek_probationary().cloned() {
						let (victim_weight, victim_hash) = match self.entries.get(&victim_key) {
							Some(e) => (e.weight, victim_key.hash),
							None => {
								// Inconsistent state, remove from segment
								self.slru.pop_probationary();
								continue;
							}
						};

						// TinyLFU admission decision: compare frequency / weight
						let candidate_score = Self::eviction_score(
							sketch.frequency(candidate_hash),
							candidate_weight,
						);
						let victim_score =
							Self::eviction_score(sketch.frequency(victim_hash), victim_weight);

						if candidate_score > victim_score {
							// Admit candidate, evict victim
							self.slru.pop_probationary();
							self.entries.remove(&victim_key);
							self.slru.insert_probationary(candidate_key.clone(), candidate_size);
							if let Some(candidate) = self.entries.get(&candidate_key) {
								candidate.set_segment(Segment::Probationary);
							}
							evictions += 1;
						} else {
							// Reject candidate
							self.entries.remove(&candidate_key);
							evictions += 1;
						}
					} else {
						// No victim available, admit directly
						self.slru.insert_probationary(candidate_key.clone(), candidate_size);
						if let Some(candidate) = self.entries.get(&candidate_key) {
							candidate.set_segment(Segment::Probationary);
						}
					}
				}
			} else {
				break;
			}
		}

		evictions
	}

	/// Evict one entry using weight-based scoring.
	///
	/// Returns the evicted key and size, or None if shard is empty.
	pub fn evict_one(&mut self, sketch: &FrequencySketch) -> Option<(ErasedKey, usize)> {
		// Sample candidates from each segment
		const SAMPLE_SIZE: usize = 5;

		let mut best_key: Option<ErasedKey> = None;
		let mut best_score = f64::MAX;

		// Sample from probationary (prefer evicting from here)
		for i in 0..SAMPLE_SIZE {
			if let Some(key) = self.slru.sample_probationary(i)
				&& let Some(entry) = self.entries.get(key)
			{
				let score = Self::eviction_score(sketch.frequency(key.hash), entry.weight);
				if score < best_score {
					best_score = score;
					best_key = Some(key.clone());
				}
			}
		}

		// If probationary is empty or we want more samples, try protected
		if best_key.is_none() || self.slru.probationary_size() == 0 {
			for i in 0..SAMPLE_SIZE {
				if let Some(key) = self.slru.sample_protected(i)
					&& let Some(entry) = self.entries.get(key)
				{
					let score = Self::eviction_score(sketch.frequency(key.hash), entry.weight);
					if score < best_score {
						best_score = score;
						best_key = Some(key.clone());
					}
				}
			}
		}

		// If main cache is empty, try window
		if best_key.is_none()
			&& let Some(key) = self.window.peek_front()
		{
			best_key = Some(key.clone());
		}

		// Evict the chosen victim
		if let Some(key) = best_key
			&& let Some(entry) = self.remove(&key)
		{
			return Some((key, entry.size));
		}

		None
	}

	/// Sample a single eviction candidate for cross-shard eviction.
	///
	/// Returns the key and its eviction score.
	pub fn sample_eviction_candidate(&self, sketch: &FrequencySketch) -> Option<EvictionCandidate> {
		// Try probationary first (most likely to be evicted)
		if let Some(key) = self.slru.sample_probationary(0)
			&& let Some(entry) = self.entries.get(key)
		{
			let score = Self::eviction_score(sketch.frequency(key.hash), entry.weight);
			return Some(EvictionCandidate {
				key: key.clone(),
				score,
			});
		}

		// Try protected
		if let Some(key) = self.slru.sample_protected(0)
			&& let Some(entry) = self.entries.get(key)
		{
			let score = Self::eviction_score(sketch.frequency(key.hash), entry.weight);
			return Some(EvictionCandidate {
				key: key.clone(),
				score,
			});
		}

		// Try window
		if let Some(key) = self.window.peek_front()
			&& let Some(entry) = self.entries.get(key)
		{
			let score = Self::eviction_score(sketch.frequency(key.hash), entry.weight);
			return Some(EvictionCandidate {
				key: key.clone(),
				score,
			});
		}

		None
	}

	/// Calculate eviction score: frequency / weight.
	///
	/// Lower score = higher eviction priority.
	fn eviction_score(frequency: u8, weight: u64) -> f64 {
		if weight == 0 {
			return f64::MAX;
		}
		(frequency as f64) / (weight as f64)
	}
}

/// Candidate for eviction with its score.
#[derive(Clone)]
pub struct EvictionCandidate {
	pub key: ErasedKey,
	pub score: f64,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::traits::{CacheKey, CacheValue};

	#[derive(Hash, Eq, PartialEq, Clone, Debug)]
	struct TestKey(u64);

	impl CacheKey for TestKey {
		type Value = TestValue;
	}

	struct TestValue {
		size: usize,
		weight: u64,
	}

	impl CacheValue for TestValue {
		fn deep_size(&self) -> usize {
			self.size
		}
		fn weight(&self) -> u64 {
			self.weight
		}
	}

	fn make_key(id: u64) -> ErasedKey {
		ErasedKey::new(&TestKey(id))
	}

	fn make_entry(size: usize, weight: u64) -> Entry {
		Entry::new(TestValue {
			size,
			weight,
		})
	}

	#[test]
	fn test_shard_insert() {
		let mut shard = Shard::new(100, 200, 800);

		let key = make_key(1);
		let entry = make_entry(50, 10);

		assert!(shard.insert(key.clone(), entry).is_none());
		assert!(shard.contains(&key));
		assert_eq!(shard.len(), 1);
	}

	#[test]
	fn test_shard_remove() {
		let mut shard = Shard::new(100, 200, 800);

		let key = make_key(1);
		let entry = make_entry(50, 10);

		shard.insert(key.clone(), entry);
		assert!(shard.remove(&key).is_some());
		assert!(!shard.contains(&key));
		assert_eq!(shard.len(), 0);
	}

	#[test]
	fn test_eviction_score() {
		// Higher frequency, same weight = higher score (less likely to evict)
		let score1 = Shard::eviction_score(10, 100);
		let score2 = Shard::eviction_score(5, 100);
		assert!(score1 > score2);

		// Same frequency, higher weight = lower score (less likely to evict)
		let score1 = Shard::eviction_score(10, 200);
		let score2 = Shard::eviction_score(10, 100);
		assert!(score1 < score2);
	}

	#[test]
	fn test_admit_from_window() {
		let mut shard = Shard::new(100, 500, 1000);
		let sketch = FrequencySketch::new(1024);

		// Insert entries into window (exceeding capacity)
		for i in 0..5 {
			let key = make_key(i);
			let entry = make_entry(50, 10);
			shard.insert(key, entry);
		}

		// Admit from window
		shard.admit_from_window(&sketch);

		// Window should be under capacity
		assert!(!shard.window.is_full());
	}

	#[test]
	fn test_promotion() {
		let mut shard = Shard::new(100, 500, 1000);

		let key = make_key(1);
		let entry = make_entry(50, 10);

		shard.insert(key.clone(), entry);

		// Move to probationary
		shard.window.remove(&key, 50);
		shard.slru.insert_probationary(key.clone(), 50);
		if let Some(e) = shard.entries.get(&key) {
			e.set_segment(Segment::Probationary);
		}

		// Promote to protected
		assert!(shard.promote(&key));

		if let Some(e) = shard.entries.get(&key) {
			assert_eq!(e.get_segment(), Segment::Protected);
		}
	}
}

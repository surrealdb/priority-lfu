use indexmap::IndexMap;

use crate::erased::ErasedKey;

/// Admission window for W-TinyLFU.
///
/// A small LRU holding recently inserted items (~1% of total cache capacity).
/// Items in the window are candidates for admission to the main cache.
pub struct Window {
	/// Ordered map: insertion order for LRU
	/// We only store keys; values are in the main entry map
	entries: IndexMap<ErasedKey, ()>,
	/// Current size in bytes
	pub(crate) size: usize,
	/// Max size (1% of total cache capacity)
	max_size: usize,
}

impl Window {
	/// Create a new window with the given maximum size.
	pub fn new(max_size: usize) -> Self {
		Self {
			entries: IndexMap::new(),
			size: 0,
			max_size,
		}
	}

	/// Insert a key with its size.
	///
	/// Returns true if the key was newly inserted.
	pub fn insert(&mut self, key: ErasedKey, size: usize) -> bool {
		self.size += size;
		self.entries.insert(key, ()).is_none()
	}

	/// Remove a key and return its position if it existed.
	pub fn remove(&mut self, key: &ErasedKey, size: usize) -> bool {
		if self.entries.shift_remove(key).is_some() {
			self.size = self.size.saturating_sub(size);
			true
		} else {
			false
		}
	}

	/// Pop the oldest (front) entry.
	///
	/// Returns the key if the window is not empty.
	pub fn pop_front(&mut self) -> Option<ErasedKey> {
		self.entries.shift_remove_index(0).map(|(key, _)| key)
	}

	/// Get the oldest key without removing it.
	pub fn peek_front(&self) -> Option<&ErasedKey> {
		self.entries.get_index(0).map(|(key, _)| key)
	}

	/// Check if window is over capacity.
	pub fn is_full(&self) -> bool {
		self.size > self.max_size
	}

	#[cfg(test)]
	pub fn contains(&self, key: &ErasedKey) -> bool {
		self.entries.contains_key(key)
	}

	#[cfg(test)]
	pub fn size(&self) -> usize {
		self.size
	}

	#[cfg(test)]
	pub fn len(&self) -> usize {
		self.entries.len()
	}

	/// Clear all entries.
	pub fn clear(&mut self) {
		self.entries.clear();
		self.size = 0;
	}
}

/// Segmented LRU (SLRU) main cache.
///
/// Two segments:
/// - Probationary (20%): New admissions from window
/// - Protected (80%): Promoted on second access
pub struct SegmentedLRU {
	/// Probationary segment
	probationary: IndexMap<ErasedKey, ()>,
	probationary_size: usize,
	#[allow(dead_code)]
	probationary_max: usize,

	/// Protected segment
	protected: IndexMap<ErasedKey, ()>,
	protected_size: usize,
	#[allow(dead_code)]
	protected_max: usize,
}

impl SegmentedLRU {
	/// Create a new SLRU with the given maximum sizes.
	///
	/// # Arguments
	///
	/// * `probationary_max` - Max size for probationary segment (typically 20% of main cache)
	/// * `protected_max` - Max size for protected segment (typically 80% of main cache)
	pub fn new(probationary_max: usize, protected_max: usize) -> Self {
		Self {
			probationary: IndexMap::new(),
			probationary_size: 0,
			probationary_max,
			protected: IndexMap::new(),
			protected_size: 0,
			protected_max,
		}
	}

	/// Insert into probationary segment.
	pub fn insert_probationary(&mut self, key: ErasedKey, size: usize) -> bool {
		self.probationary_size += size;
		self.probationary.insert(key, ()).is_none()
	}

	/// Remove from probationary segment.
	pub fn remove_probationary(&mut self, key: &ErasedKey, size: usize) -> bool {
		if self.probationary.shift_remove(key).is_some() {
			self.probationary_size = self.probationary_size.saturating_sub(size);
			true
		} else {
			false
		}
	}

	/// Remove from protected segment.
	pub fn remove_protected(&mut self, key: &ErasedKey, size: usize) -> bool {
		if self.protected.shift_remove(key).is_some() {
			self.protected_size = self.protected_size.saturating_sub(size);
			true
		} else {
			false
		}
	}

	/// Promote a key from probationary to protected.
	///
	/// Returns true if the key was found and promoted.
	pub fn promote(&mut self, key: &ErasedKey, size: usize) -> bool {
		if self.probationary.shift_remove(key).is_some() {
			self.probationary_size = self.probationary_size.saturating_sub(size);
			self.protected_size += size;
			self.protected.insert(key.clone(), ());
			true
		} else {
			false
		}
	}

	/// Pop the oldest entry from probationary.
	pub fn pop_probationary(&mut self) -> Option<ErasedKey> {
		self.probationary.shift_remove_index(0).map(|(key, _)| key)
	}

	/// Get the oldest key from probationary without removing it.
	pub fn peek_probationary(&self) -> Option<&ErasedKey> {
		self.probationary.get_index(0).map(|(key, _)| key)
	}

	/// Get a random entry from probationary for eviction sampling.
	pub fn sample_probationary(&self, index: usize) -> Option<&ErasedKey> {
		if self.probationary.is_empty() {
			return None;
		}
		let idx = index % self.probationary.len();
		self.probationary.get_index(idx).map(|(key, _)| key)
	}

	/// Get a random entry from protected for eviction sampling.
	pub fn sample_protected(&self, index: usize) -> Option<&ErasedKey> {
		if self.protected.is_empty() {
			return None;
		}
		let idx = index % self.protected.len();
		self.protected.get_index(idx).map(|(key, _)| key)
	}

	/// Total size across both segments.
	#[cfg(test)]
	pub fn total_size(&self) -> usize {
		self.probationary_size + self.protected_size
	}

	/// Probationary segment size.
	pub fn probationary_size(&self) -> usize {
		self.probationary_size
	}

	/// Check if probationary is over capacity.
	pub fn probationary_is_full(&self) -> bool {
		self.probationary_size > self.probationary_max
	}

	#[cfg(test)]
	pub fn probationary_contains(&self, key: &ErasedKey) -> bool {
		self.probationary.contains_key(key)
	}

	#[cfg(test)]
	pub fn protected_contains(&self, key: &ErasedKey) -> bool {
		self.protected.contains_key(key)
	}

	#[cfg(test)]
	pub fn protected_size(&self) -> usize {
		self.protected_size
	}

	#[cfg(test)]
	pub fn insert_protected(&mut self, key: ErasedKey, size: usize) -> bool {
		self.protected_size += size;
		self.protected.insert(key, ()).is_none()
	}

	#[cfg(test)]
	pub fn demote(&mut self, key: &ErasedKey, size: usize) -> bool {
		if self.protected.shift_remove(key).is_some() {
			self.protected_size = self.protected_size.saturating_sub(size);
			self.probationary_size += size;
			self.probationary.insert(key.clone(), ());
			true
		} else {
			false
		}
	}

	/// Clear all segments.
	pub fn clear(&mut self) {
		self.probationary.clear();
		self.probationary_size = 0;
		self.protected.clear();
		self.protected_size = 0;
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::DeepSizeOf;
	use crate::traits::CacheKey;

	#[derive(Hash, Eq, PartialEq, Clone, Debug)]
	struct TestKey(u64);

	impl CacheKey for TestKey {
		type Value = TestValue;

		fn weight(&self) -> u64 {
			50
		}
	}

	#[derive(DeepSizeOf)]
	struct TestValue;

	fn make_key(id: u64) -> ErasedKey {
		ErasedKey::new(&TestKey(id))
	}

	#[test]
	fn test_window_basic() {
		let mut window = Window::new(1000);

		let key1 = make_key(1);
		let key2 = make_key(2);

		assert!(window.insert(key1.clone(), 100));
		assert_eq!(window.size(), 100);
		assert_eq!(window.len(), 1);

		assert!(window.insert(key2.clone(), 200));
		assert_eq!(window.size(), 300);
		assert_eq!(window.len(), 2);

		assert!(window.contains(&key1));
		assert!(window.contains(&key2));
	}

	#[test]
	fn test_window_pop_front() {
		let mut window = Window::new(1000);

		let key1 = make_key(1);
		let key2 = make_key(2);

		window.insert(key1.clone(), 100);
		window.insert(key2.clone(), 200);

		let popped = window.pop_front();
		assert!(popped.is_some());
		assert_eq!(window.len(), 1);
	}

	#[test]
	fn test_window_is_full() {
		let mut window = Window::new(250);

		window.insert(make_key(1), 100);
		assert!(!window.is_full());

		window.insert(make_key(2), 200);
		assert!(window.is_full());
	}

	#[test]
	fn test_slru_basic() {
		let mut slru = SegmentedLRU::new(200, 800);

		let key1 = make_key(1);
		let key2 = make_key(2);

		assert!(slru.insert_probationary(key1.clone(), 100));
		assert_eq!(slru.probationary_size(), 100);

		assert!(slru.insert_protected(key2.clone(), 150));
		assert_eq!(slru.protected_size(), 150);
		assert_eq!(slru.total_size(), 250);
	}

	#[test]
	fn test_slru_promotion() {
		let mut slru = SegmentedLRU::new(200, 800);

		let key = make_key(1);

		slru.insert_probationary(key.clone(), 100);
		assert!(slru.probationary_contains(&key));
		assert!(!slru.protected_contains(&key));

		assert!(slru.promote(&key, 100));
		assert!(!slru.probationary_contains(&key));
		assert!(slru.protected_contains(&key));
		assert_eq!(slru.probationary_size(), 0);
		assert_eq!(slru.protected_size(), 100);
	}

	#[test]
	fn test_slru_demotion() {
		let mut slru = SegmentedLRU::new(200, 800);

		let key = make_key(1);

		slru.insert_protected(key.clone(), 100);
		assert!(slru.protected_contains(&key));

		assert!(slru.demote(&key, 100));
		assert!(slru.probationary_contains(&key));
		assert!(!slru.protected_contains(&key));
	}

	#[test]
	fn test_slru_sampling() {
		let mut slru = SegmentedLRU::new(500, 1000);

		for i in 0..5 {
			slru.insert_probationary(make_key(i), 50);
		}

		// Should be able to sample
		assert!(slru.sample_probationary(0).is_some());
		assert!(slru.sample_probationary(2).is_some());
		assert!(slru.sample_probationary(100).is_some()); // Wraps around
	}
}

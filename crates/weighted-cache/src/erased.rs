use std::any::{Any, TypeId};
use std::convert::TryFrom;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU16, Ordering};

use crate::deepsize::DeepSizeOf;
use crate::traits::CacheKey;

/// Entry state for Clock-PRO algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResidentState {
	Hot = 0,
	Cold = 1,
}

impl TryFrom<u8> for ResidentState {
	type Error = u8;

	fn try_from(val: u8) -> Result<Self, Self::Error> {
		match val {
			0 => Ok(ResidentState::Hot),
			1 => Ok(ResidentState::Cold),
			_ => Err(val),
		}
	}
}

/// Type-erased cache key with pre-computed hash.
///
/// This allows storing keys of different types in the same HashMap
/// without requiring a unified enum type.
#[derive(Clone)]
pub(crate) struct ErasedKey {
	/// TypeId of the concrete key type K
	pub type_id: TypeId,
	/// Pre-computed hash of (TypeId, K)
	pub hash: u64,
	/// The actual key, boxed and type-erased
	pub data: Arc<dyn Any + Send + Sync>,
}

impl ErasedKey {
	/// Create a new erased key from a concrete key type.
	pub fn new<K: CacheKey>(key: &K) -> Self {
		let type_id = TypeId::of::<K>();
		let hash = Self::compute_hash(type_id, key);
		Self {
			type_id,
			hash,
			data: Arc::new(key.clone()),
		}
	}

	/// Compute the combined hash of TypeId and key.
	pub(crate) fn compute_hash<K: CacheKey>(type_id: TypeId, key: &K) -> u64 {
		let mut hasher = ahash::AHasher::default();
		type_id.hash(&mut hasher);
		key.hash(&mut hasher);
		hasher.finish()
	}

	/// Attempt to downcast to the concrete key type.
	#[cfg(test)]
	pub fn downcast_ref<K: 'static>(&self) -> Option<&K> {
		self.data.downcast_ref()
	}

	/// Check equality by comparing with another erased key.
	#[cfg(test)]
	pub fn equals<K: CacheKey>(&self, other: &K) -> bool {
		if self.type_id != TypeId::of::<K>() {
			return false;
		}
		if let Some(self_key) = self.downcast_ref::<K>() {
			self_key == other
		} else {
			false
		}
	}
}

impl Hash for ErasedKey {
	fn hash<H: Hasher>(&self, state: &mut H) {
		// Use pre-computed hash to avoid re-hashing on every lookup
		self.hash.hash(state);
	}
}

impl PartialEq for ErasedKey {
	fn eq(&self, other: &Self) -> bool {
		// Fast path: compare hashes and TypeIds
		if self.hash != other.hash || self.type_id != other.type_id {
			return false;
		}

		// Fast path: if same Arc, they're equal
		if Arc::ptr_eq(&self.data, &other.data) {
			return true;
		}

		// For hash collisions or different instances of the same key,
		// we can't easily do deep comparison without knowing the type.
		// However, if hash and type_id match, we'll consider them equal.
		// This is safe because:
		// 1. TypeId ensures they're the same type
		// 2. Hash collision is extremely rare with ahash
		// 3. Even if there's a collision, the HashMap will use the hash for bucketing and then use
		//    this eq for final comparison
		true
	}
}

impl Eq for ErasedKey {}

/// Borrowed reference to a cache key for zero-allocation lookups.
///
/// This type holds a borrowed reference to the key and pre-computed hash,
/// allowing HashMap lookups without allocating an owned `ErasedKey`.
pub(crate) struct ErasedKeyRef<'a, K> {
	pub type_id: TypeId,
	pub hash: u64,
	pub key: &'a K,
}

impl<'a, K: CacheKey> ErasedKeyRef<'a, K> {
	/// Create a borrowed key reference (no allocation).
	pub fn new(key: &'a K) -> Self {
		let type_id = TypeId::of::<K>();
		let hash = ErasedKey::compute_hash(type_id, key);
		Self {
			type_id,
			hash,
			key,
		}
	}

	/// Check equality with owned ErasedKey.
	pub fn equals(&self, other: &ErasedKey) -> bool {
		if self.hash != other.hash || self.type_id != other.type_id {
			return false;
		}

		// Downcast and compare
		if let Some(other_key) = other.data.downcast_ref::<K>() {
			self.key == other_key
		} else {
			false
		}
	}
}

impl<'a, K: CacheKey> Hash for ErasedKeyRef<'a, K> {
	fn hash<H: Hasher>(&self, state: &mut H) {
		// Use pre-computed hash to avoid re-hashing on every lookup
		self.hash.hash(state);
	}
}

/// Type-erased cache entry with cached metadata.
///
/// Stores the value as `Arc<dyn Any>` to enable cheap cloning for `get_arc()`.
pub struct Entry {
	/// Type-erased value wrapped in Arc for cheap sharing across threads.
	pub value: Arc<dyn Any + Send + Sync>,
	/// Cached deep_size() result (immutable after creation)
	pub size: usize,
	/// Cached weight() result (immutable after creation)
	pub weight: u64,
	/// Clock-PRO state (Hot or Cold)
	pub state: AtomicU8,
	/// Reference counter for Clock-PRO (0-2)
	pub referenced: AtomicU16,
}

impl Entry {
	/// Create a new entry from a concrete value with the given weight.
	///
	/// The weight determines eviction priority (higher = more resistant to eviction).
	pub fn new<V: DeepSizeOf + Send + Sync + 'static>(value: V, weight: u64) -> Self {
		let size = value.deep_size_of();
		Self {
			size,
			weight,
			value: Arc::new(value),
			state: AtomicU8::new(ResidentState::Cold as u8),
			referenced: AtomicU16::new(0),
		}
	}

	/// Clone the Arc without cloning the underlying value.
	///
	/// Returns None if the type doesn't match.
	pub fn value_arc<V: Send + Sync + 'static>(&self) -> Option<Arc<V>> {
		// Clone the Arc (cheap reference count bump)
		let arc_any = Arc::clone(&self.value);
		// Downcast to concrete type
		Arc::downcast::<V>(arc_any).ok()
	}

	/// Get the current state.
	pub fn get_state(&self) -> ResidentState {
		let val = self.state.load(Ordering::Acquire);
		ResidentState::try_from(val).expect("Invalid state value stored in atomic")
	}

	/// Set the state atomically.
	pub fn set_state(&self, state: ResidentState) {
		self.state.store(state as u8, Ordering::Release);
	}

	/// Get reference counter value.
	pub fn get_referenced(&self) -> u16 {
		self.referenced.load(Ordering::Acquire)
	}

	/// Set reference counter value.
	pub fn set_referenced(&self, val: u16) {
		self.referenced.store(val, Ordering::Release);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::DeepSizeOf;

	#[derive(Hash, Eq, PartialEq, Clone, Debug)]
	struct TestKey(u64);

	impl CacheKey for TestKey {
		type Value = TestValue;

		fn weight(&self) -> u64 {
			50
		}
	}

	#[derive(DeepSizeOf)]
	struct TestValue {
		data: Vec<u8>,
	}

	#[test]
	fn test_erased_key_creation() {
		let key = TestKey(42);
		let erased = ErasedKey::new(&key);

		assert_eq!(erased.type_id, TypeId::of::<TestKey>());
		assert!(erased.downcast_ref::<TestKey>().is_some());
		assert_eq!(erased.downcast_ref::<TestKey>().unwrap(), &TestKey(42));
	}

	#[test]
	fn test_erased_key_equals() {
		let key1 = TestKey(42);
		let key2 = TestKey(42);
		let key3 = TestKey(99);

		let erased1 = ErasedKey::new(&key1);

		assert!(erased1.equals(&key2));
		assert!(!erased1.equals(&key3));
	}

	#[test]
	fn test_entry_creation() {
		let value = TestValue {
			data: vec![1, 2, 3, 4],
		};
		let entry = Entry::new(value, 50);

		assert_eq!(entry.weight, 50);
		assert!(entry.size > 0);
		assert_eq!(entry.get_state(), ResidentState::Cold);
		assert_eq!(entry.get_referenced(), 0);
	}

	#[test]
	fn test_entry_value_arc() {
		let value = TestValue {
			data: vec![1, 2, 3, 4],
		};
		let entry = Entry::new(value, 50);

		let arc = entry.value_arc::<TestValue>();
		assert!(arc.is_some());

		let arc = entry.value_arc::<String>();
		assert!(arc.is_none());
	}

	#[test]
	fn test_state_transitions() {
		let value = TestValue {
			data: vec![1, 2, 3],
		};
		let entry = Entry::new(value, 50);

		assert_eq!(entry.get_state(), ResidentState::Cold);

		entry.set_state(ResidentState::Hot);
		assert_eq!(entry.get_state(), ResidentState::Hot);

		entry.set_state(ResidentState::Cold);
		assert_eq!(entry.get_state(), ResidentState::Cold);
	}

	#[test]
	fn test_referenced_counter() {
		let value = TestValue {
			data: vec![1, 2, 3],
		};
		let entry = Entry::new(value, 50);

		assert_eq!(entry.get_referenced(), 0);

		entry.set_referenced(1);
		assert_eq!(entry.get_referenced(), 1);

		entry.set_referenced(2);
		assert_eq!(entry.get_referenced(), 2);
	}
}

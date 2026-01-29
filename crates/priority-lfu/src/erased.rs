use std::any::{Any, TypeId};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8};

use crate::deepsize::DeepSizeOf;
use crate::traits::{CacheKey, CachePolicy};

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
	/// The actual key, boxed and type-erased (Arc allows cheap cloning for policy buckets)
	pub data: Arc<dyn Any + Send + Sync>,
	/// Equality comparison function for handling hash collisions correctly
	eq_fn: fn(&Arc<dyn Any + Send + Sync>, &Arc<dyn Any + Send + Sync>) -> bool,
}

impl ErasedKey {
	/// Create a new erased key from a concrete key type.
	pub fn new<K: CacheKey>(key: &K) -> Self {
		let type_id = TypeId::of::<K>();
		let hash = Self::compute_hash(type_id, key);

		// Create a monomorphized equality function for this key type.
		// The compiler will generate one instance per concrete K type.
		fn eq_impl<K: CacheKey>(
			a: &Arc<dyn Any + Send + Sync>,
			b: &Arc<dyn Any + Send + Sync>,
		) -> bool {
			match (a.downcast_ref::<K>(), b.downcast_ref::<K>()) {
				(Some(a_key), Some(b_key)) => a_key == b_key,
				_ => false,
			}
		}

		Self {
			type_id,
			hash,
			data: Arc::new(key.clone()),
			eq_fn: eq_impl::<K>,
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
		// Fast path: different hash or type means not equal
		if self.hash != other.hash || self.type_id != other.type_id {
			return false;
		}

		// Fast path: if same Arc pointer, they're definitely equal
		if Arc::ptr_eq(&self.data, &other.data) {
			return true;
		}

		// Proper comparison using stored equality function.
		// This correctly handles hash collisions by comparing actual keys.
		(self.eq_fn)(&self.data, &other.data)
	}
}

impl Eq for ErasedKey {}

impl std::fmt::Debug for ErasedKey {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ErasedKey")
			.field("type_id", &self.type_id)
			.field("hash", &self.hash)
			.field("data", &"<Arc<dyn Any>>")
			.field("eq_fn", &"<fn>")
			.finish()
	}
}

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
/// Stores the value as `Box<dyn Any>` for type erasure without forced Arc overhead.
/// Users control ownership semantics via their `CacheKey::Value` type (e.g., `Arc<T>`).
pub struct Entry {
	/// Type-erased value
	pub value: Box<dyn Any + Send + Sync>,
	/// Cached deep_size() result (immutable after creation)
	pub size: usize,
	/// Cached policy() result (immutable after creation)
	pub policy: CachePolicy,
	/// LFU frequency counter (0-255)
	pub frequency: AtomicU8,
	/// Clock reference bit
	pub clock_bit: AtomicBool,
}

impl Entry {
	/// Create a new entry from a concrete value with the given policy.
	///
	/// The policy determines eviction priority (lower discriminant = more resistant to eviction).
	pub fn new<V: DeepSizeOf + Send + Sync + 'static>(value: V, policy: CachePolicy) -> Self {
		let size = value.deep_size_of();
		Self {
			size,
			policy,
			value: Box::new(value),
			frequency: AtomicU8::new(0),
			clock_bit: AtomicBool::new(false),
		}
	}

	/// Get a reference to the value.
	///
	/// Returns None if the type doesn't match.
	pub fn value_ref<V: Send + Sync + 'static>(&self) -> Option<&V> {
		self.value.downcast_ref::<V>()
	}

	/// Consume the entry and extract the value.
	///
	/// Returns None if the type doesn't match.
	pub fn into_value<V: Send + Sync + 'static>(self) -> Option<V> {
		self.value.downcast::<V>().ok().map(|boxed| *boxed)
	}
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::Ordering;

	use super::*;
	use crate::DeepSizeOf;

	#[derive(Hash, Eq, PartialEq, Clone, Debug)]
	struct TestKey(u64);

	impl CacheKey for TestKey {
		type Value = TestValue;
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
		assert_eq!(erased.downcast_ref::<TestKey>().expect("should downcast"), &TestKey(42));
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
	fn test_erased_key_partialeq() {
		// Test that PartialEq correctly distinguishes different keys
		let key1 = TestKey(42);
		let key2 = TestKey(42);
		let key3 = TestKey(99);

		let erased1 = ErasedKey::new(&key1);
		let erased2 = ErasedKey::new(&key2);
		let erased3 = ErasedKey::new(&key3);

		// Same value keys should be equal
		assert_eq!(erased1, erased2);

		// Different value keys should not be equal
		assert_ne!(erased1, erased3);
		assert_ne!(erased2, erased3);
	}

	#[test]
	fn test_erased_key_arc_ptr_eq_optimization() {
		// Test that Arc::ptr_eq fast path works
		let key = TestKey(42);
		let erased = ErasedKey::new(&key);
		let erased_clone = erased.clone();

		// Cloned ErasedKey should be equal (Arc pointers are the same)
		assert_eq!(erased, erased_clone);
		assert!(Arc::ptr_eq(&erased.data, &erased_clone.data));
	}

	#[test]
	fn test_entry_creation() {
		let value = TestValue {
			data: vec![1, 2, 3, 4],
		};
		let entry = Entry::new(value, CachePolicy::Standard);

		assert_eq!(entry.policy, CachePolicy::Standard);
		assert!(entry.size > 0);
		assert_eq!(entry.frequency.load(Ordering::Relaxed), 0);
		assert_eq!(entry.clock_bit.load(Ordering::Relaxed), false);
	}

	#[test]
	fn test_entry_value_ref() {
		let value = TestValue {
			data: vec![1, 2, 3, 4],
		};
		let entry = Entry::new(value, CachePolicy::Standard);

		let value_ref = entry.value_ref::<TestValue>();
		assert!(value_ref.is_some());
		assert_eq!(value_ref.expect("value_ref should be Some").data, vec![1, 2, 3, 4]);

		let value_ref = entry.value_ref::<String>();
		assert!(value_ref.is_none());
	}

	#[test]
	fn test_entry_into_value() {
		let value = TestValue {
			data: vec![1, 2, 3, 4],
		};
		let entry = Entry::new(value, CachePolicy::Standard);

		let extracted = entry.into_value::<TestValue>();
		assert!(extracted.is_some());
		assert_eq!(extracted.expect("extracted should be Some").data, vec![1, 2, 3, 4]);
	}

	#[test]
	fn test_clock_bit() {
		let value = TestValue {
			data: vec![1, 2, 3],
		};
		let entry = Entry::new(value, CachePolicy::Standard);

		assert_eq!(entry.clock_bit.load(Ordering::Relaxed), false);

		entry.clock_bit.store(true, Ordering::Relaxed);
		assert_eq!(entry.clock_bit.load(Ordering::Relaxed), true);

		entry.clock_bit.store(false, Ordering::Relaxed);
		assert_eq!(entry.clock_bit.load(Ordering::Relaxed), false);
	}

	#[test]
	fn test_frequency_counter() {
		let value = TestValue {
			data: vec![1, 2, 3],
		};
		let entry = Entry::new(value, CachePolicy::Standard);

		assert_eq!(entry.frequency.load(Ordering::Relaxed), 0);

		entry.frequency.store(5, Ordering::Relaxed);
		assert_eq!(entry.frequency.load(Ordering::Relaxed), 5);

		entry.frequency.store(255, Ordering::Relaxed);
		assert_eq!(entry.frequency.load(Ordering::Relaxed), 255);
	}
}

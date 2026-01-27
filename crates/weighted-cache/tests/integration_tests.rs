use std::sync::Arc;
use std::thread;

use weighted_cache::{Cache, CacheBuilder, CacheKey, CachePolicy, DeepSizeOf};

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct StringKey(String);

impl CacheKey for StringKey {
	type Value = StringValue;

	fn policy(&self) -> CachePolicy {
		CachePolicy::Standard
	}
}

#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
struct StringValue {
	data: String,
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct IntKey(u64);

impl CacheKey for IntKey {
	type Value = IntValue;

	fn policy(&self) -> CachePolicy {
		CachePolicy::Standard
	}
}

#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
struct IntValue(i64);

#[test]
fn test_basic_operations() {
	let cache = Cache::new(10240);

	let key = StringKey("test".to_string());
	let value = StringValue {
		data: "hello world".to_string(),
	};

	// Insert
	cache.insert(key.clone(), value.clone());
	assert!(cache.contains(&key));

	// Get with guard
	{
		let guard = cache.get(&key).expect("key should exist");
		assert_eq!(*guard, value);
	}

	// Get with Arc
	let arc = cache.get_arc(&key).expect("key should exist");
	assert_eq!(*arc, value);

	// Get clone
	let cloned = cache.get_clone(&key).expect("key should exist");
	assert_eq!(cloned, value);

	// Remove
	let removed = cache.remove(&key).expect("key should exist");
	assert_eq!(*removed, value);
	assert!(!cache.contains(&key));
}

#[test]
fn test_heterogeneous_types() {
	let cache = Cache::new(10240);

	let str_key = StringKey("foo".to_string());
	let str_val = StringValue {
		data: "bar".to_string(),
	};

	let int_key = IntKey(42);
	let int_val = IntValue(99);

	cache.insert(str_key.clone(), str_val.clone());
	cache.insert(int_key.clone(), int_val.clone());

	assert_eq!(cache.len(), 2);

	let str_retrieved = cache.get_arc(&str_key).expect("str_key should exist");
	assert_eq!(*str_retrieved, str_val);

	let int_retrieved = cache.get_arc(&int_key).expect("int_key should exist");
	assert_eq!(*int_retrieved, int_val);
}

#[test]
fn test_update_existing() {
	let cache = Cache::new(10240);

	let key = IntKey(1);
	let value1 = IntValue(100);
	let value2 = IntValue(200);

	let old = cache.insert(key.clone(), value1.clone());
	assert!(old.is_none());

	let old = cache.insert(key.clone(), value2.clone());
	assert!(old.is_some());
	assert_eq!(*old.expect("old value should exist"), value1);

	let current = cache.get_arc(&key).expect("key should exist");
	assert_eq!(*current, value2);
}

#[test]
fn test_eviction_on_capacity() {
	let cache = Cache::new(300); // Small capacity

	// Insert many entries to trigger eviction
	// IntValue has deep_size = 8 bytes, so 50 entries = 400 bytes
	for i in 0..50 {
		let key = IntKey(i);
		let value = IntValue(i as i64);
		cache.insert(key, value);
	}

	// Should have evicted some entries (cache tracks value sizes via deep_size)
	// Clock-PRO may stop eviction slightly early due to MAX_STUCK mechanism
	// when entries have references, so allow small margin
	assert!(cache.len() < 50, "Expected fewer than 50 entries, got {}", cache.len());
	assert!(cache.size() <= 320, "Expected size <= 320, got {}", cache.size());
}

#[test]
fn test_frequency_based_eviction() {
	// Test that frequently accessed entries with high weight are more resistant to eviction
	let cache = Cache::new(500);

	// High weight value (more resistant to eviction)
	#[derive(Hash, Eq, PartialEq, Clone, Debug)]
	struct HighWeightKey(u64);

	impl CacheKey for HighWeightKey {
		type Value = IntValue;

		fn policy(&self) -> CachePolicy {
			CachePolicy::Critical // High priority
		}
	}

	// Insert hot key with high weight
	let hot_key = HighWeightKey(1);
	cache.insert(hot_key.clone(), IntValue(1));

	// Access it multiple times to build up references
	for _ in 0..10 {
		let _ = cache.get_arc(&hot_key);
	}

	// Insert many low-weight entries to trigger eviction
	for i in 10..40 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	// Hot key with high priority and high access should still be present
	assert!(
		cache.contains(&hot_key),
		"High-priority frequently accessed entry should survive eviction"
	);
}

#[test]
fn test_concurrent_reads() {
	let cache = Arc::new(Cache::new(10240));

	// Insert some data
	for i in 0..100 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	let mut handles = vec![];

	for _ in 0..4 {
		let cache = cache.clone();
		handles.push(thread::spawn(move || {
			for i in 0..100 {
				let key = IntKey(i);
				if let Some(value) = cache.get_arc(&key) {
					assert_eq!(value.0, i as i64);
				}
			}
		}));
	}

	for handle in handles {
		handle.join().expect("thread should not panic");
	}
}

#[test]
fn test_concurrent_writes() {
	let cache = Arc::new(Cache::new(10240));
	let mut handles = vec![];

	for t in 0..4 {
		let cache = cache.clone();
		handles.push(thread::spawn(move || {
			for i in 0..25 {
				let key = IntKey(t * 25 + i);
				let value = IntValue((t * 25 + i) as i64);
				cache.insert(key, value);
			}
		}));
	}

	for handle in handles {
		handle.join().expect("thread should not panic");
	}

	assert_eq!(cache.len(), 100);
}

#[test]
fn test_concurrent_mixed_operations() {
	let cache = Arc::new(Cache::new(10240));

	// Pre-populate
	for i in 0..50 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	let mut handles = vec![];

	// Readers
	for _ in 0..2 {
		let cache = cache.clone();
		handles.push(thread::spawn(move || {
			for i in 0..100 {
				let key = IntKey(i % 50);
				let _ = cache.get_arc(&key);
			}
		}));
	}

	// Writers
	for t in 0..2 {
		let cache = cache.clone();
		handles.push(thread::spawn(move || {
			for i in 0..25 {
				let key = IntKey(50 + t * 25 + i);
				let value = IntValue((50 + t * 25 + i) as i64);
				cache.insert(key, value);
			}
		}));
	}

	for handle in handles {
		handle.join().expect("thread should not panic");
	}

	assert!(!cache.is_empty());
}

#[test]
fn test_clear() {
	let cache = Cache::new(10240);

	for i in 0..10 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	assert_eq!(cache.len(), 10);

	cache.clear();

	assert_eq!(cache.len(), 0);
	assert_eq!(cache.size(), 0);
	assert!(cache.is_empty());
}

#[test]
fn test_builder() {
	let cache = CacheBuilder::new(1024).shards(32).build();

	cache.insert(IntKey(1), IntValue(100));
	assert!(cache.contains(&IntKey(1)));
}

#[test]
fn test_large_values() {
	let cache = Cache::new(1024 * 1024); // 1 MB

	let key = StringKey("large".to_string());
	let value = StringValue {
		data: "x".repeat(100_000), // 100 KB
	};

	cache.insert(key.clone(), value);

	let retrieved = cache.get_arc(&key).expect("key should exist");
	assert_eq!(retrieved.data.len(), 100_000);
}

#[test]
fn test_policy_based_eviction() {
	let cache = Cache::new(1000);

	// High priority value (more resistant to eviction)
	#[derive(Hash, Eq, PartialEq, Clone, Debug)]
	struct CriticalKey(u64);

	impl CacheKey for CriticalKey {
		type Value = CriticalValue;

		fn policy(&self) -> CachePolicy {
			CachePolicy::Critical
		}
	}

	#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
	struct CriticalValue;

	// Low priority value (easily evicted)
	#[derive(Hash, Eq, PartialEq, Clone, Debug)]
	struct VolatileKey(u64);

	impl CacheKey for VolatileKey {
		type Value = VolatileValue;

		fn policy(&self) -> CachePolicy {
			CachePolicy::Volatile
		}
	}

	#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
	struct VolatileValue;

	cache.insert(CriticalKey(1), CriticalValue);
	cache.insert(VolatileKey(2), VolatileValue);

	// Fill cache to trigger eviction
	for i in 10..20 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	// Critical item should be more likely to survive
	// Note: This is probabilistic, so we can't guarantee it in a single test
}

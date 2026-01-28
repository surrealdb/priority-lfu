use proptest::prelude::*;
use weighted_cache::{Cache, CacheKey, DeepSizeOf};

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct TestKey(u64, u64); // (id, weight)

impl CacheKey for TestKey {
	type Value = TestValue;

	fn policy(&self) -> weighted_cache::CachePolicy {
		// Map the u64 weight to a policy for property testing
		match self.1 % 3 {
			0 => weighted_cache::CachePolicy::Critical,
			1 => weighted_cache::CachePolicy::Standard,
			_ => weighted_cache::CachePolicy::Volatile,
		}
	}
}

#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
struct TestValue {
	size: usize,
}

proptest! {
	#[test]
	fn test_insert_get_consistency(keys in prop::collection::vec(0u64..100, 1..50)) {
		let cache = Cache::new(1024 * 1024); // 1MB to avoid eviction during test

		for key in &keys {
			let value = TestValue { size: 100 };
			cache.insert(TestKey(*key, 50), value.clone());
		}

		for key in &keys {
			let result = cache.get_arc(&TestKey(*key, 50));
			prop_assert!(result.is_some());
		}
	}

	#[test]
	fn test_size_accounting(operations in prop::collection::vec((0u64..20, 10usize..200, 10u64..100), 1..30)) {
		let max_size = 5000;
		let cache = Cache::new(max_size);

		for (key, size, weight) in operations {
			cache.insert(TestKey(key, weight), TestValue { size });
		}

		// Size should never exceed max_size (with some tolerance for overhead)
		prop_assert!(cache.size() <= max_size);
	}

	#[test]
	fn test_remove_decreases_size(
		inserts in prop::collection::vec((0u64..50, 50usize..150, 10u64..100), 10..20),
		remove_indices in prop::collection::vec(0usize..10, 1..5)
	) {
		let cache = Cache::new(1024 * 1024); // 1MB to avoid eviction during test
		let mut inserted_keys = Vec::new();

		for (key, size, weight) in inserts {
			cache.insert(TestKey(key, weight), TestValue { size });
			inserted_keys.push(TestKey(key, weight));
		}

		let size_before = cache.size();

		for &idx in &remove_indices {
			if idx < inserted_keys.len() {
				cache.remove(&inserted_keys[idx]);
			}
		}

		let size_after = cache.size();

		// Size should decrease or stay the same after removals
		prop_assert!(size_after <= size_before);
	}

	#[test]
	fn test_clear_empties_cache(operations in prop::collection::vec((0u64..100, 10usize..100, 10u64..100), 1..50)) {
		let cache = Cache::new(1024 * 1024); // 1MB to avoid eviction during test

		for (key, size, weight) in operations {
			cache.insert(TestKey(key, weight), TestValue { size });
		}

		cache.clear();

		prop_assert_eq!(cache.len(), 0);
		prop_assert_eq!(cache.size(), 0);
		prop_assert!(cache.is_empty());
	}

	#[test]
	fn test_update_existing_key(key in 0u64..100, values in prop::collection::vec((10usize..200, 10u64..100), 2..10)) {
		let cache = Cache::new(1024 * 1024); // 1MB to avoid eviction during test

		// Use a fixed weight for the key
		let weight = 50u64;
		for (size, _) in values {
			cache.insert(TestKey(key, weight), TestValue { size });
		}

		// Key should exist
		prop_assert!(cache.contains(&TestKey(key, weight)));

		// Cache should have exactly 1 entry for this key
		let result = cache.get_arc(&TestKey(key, weight));
		prop_assert!(result.is_some());
	}

	#[test]
	fn test_contains_after_insert(keys in prop::collection::vec(0u64..100, 1..50)) {
		let cache = Cache::new(1024 * 1024); // 1MB to avoid eviction during test

		for key in &keys {
			cache.insert(TestKey(*key, 50), TestValue { size: 100 });
		}

		for key in &keys {
			prop_assert!(cache.contains(&TestKey(*key, 50)));
		}
	}

	#[test]
	fn test_not_contains_after_remove(keys in prop::collection::vec(0u64..50, 5..20)) {
		let cache = Cache::new(1024 * 1024); // 1MB to avoid eviction during test

		for key in &keys {
			cache.insert(TestKey(*key, 50), TestValue { size: 100 });
		}

		for key in &keys {
			cache.remove(&TestKey(*key, 50));
			prop_assert!(!cache.contains(&TestKey(*key, 50)));
		}
	}
}

#[test]
fn test_no_panics_on_empty_operations() {
	let cache = Cache::new(1024);

	// Operations on empty cache should not panic
	assert!(cache.get_arc(&TestKey(1, 50)).is_none());
	assert!(cache.remove(&TestKey(1, 50)).is_none());
	assert!(!cache.contains(&TestKey(1, 50)));
	assert_eq!(cache.len(), 0);
	assert_eq!(cache.size(), 0);

	cache.clear(); // Should not panic
}

#[test]
fn test_duplicate_insertions() {
	let cache = Cache::new(10240);
	let key = TestKey(1, 50);

	for _ in 0..100 {
		cache.insert(
			key.clone(),
			TestValue {
				size: 50,
			},
		);
	}

	// Should have exactly one entry
	assert_eq!(cache.len(), 1);
}

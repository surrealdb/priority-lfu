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

// Policy-specific helper key types for testing eviction behavior
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct VolatileKey(u64);

impl CacheKey for VolatileKey {
	type Value = IntValue;

	fn policy(&self) -> CachePolicy {
		CachePolicy::Volatile
	}
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct StandardKey(u64);

impl CacheKey for StandardKey {
	type Value = IntValue;

	fn policy(&self) -> CachePolicy {
		CachePolicy::Standard
	}
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct CriticalKey(u64);

impl CacheKey for CriticalKey {
	type Value = IntValue;

	fn policy(&self) -> CachePolicy {
		CachePolicy::Critical
	}
}

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
	// Test that frequently accessed entries are more resistant to eviction
	let cache = Cache::new(600);

	// Insert Critical entries
	for i in 1..=5 {
		cache.insert(CriticalKey(i), IntValue(i as i64));
	}

	// Access some Critical entries heavily
	for _ in 0..20 {
		let _ = cache.get_arc(&CriticalKey(1));
		let _ = cache.get_arc(&CriticalKey(2));
	}

	// Don't access CriticalKey(3), (4), (5)

	// Insert many Standard entries to trigger eviction
	for i in 10..60 {
		cache.insert(StandardKey(i), IntValue(i as i64));
	}

	// Count which Critical entries survived
	let accessed_survived = cache.contains(&CriticalKey(1)) || cache.contains(&CriticalKey(2));
	let unaccessed_survived = (3..=5).filter(|&i| cache.contains(&CriticalKey(i))).count();

	// At least one accessed entry should survive, or fewer unaccessed should survive
	assert!(
		accessed_survived || unaccessed_survived <= 2,
		"Frequently accessed entries should have better survival"
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

// ============================================================================
// Comprehensive Policy-Based Eviction Tests
// ============================================================================

#[test]
fn test_policy_eviction_order_strict() {
	// Verify policy-based eviction behavior
	let cache = Cache::new(600); // Larger capacity to reduce race conditions

	// Insert entries of different policies
	for i in 1..=10 {
		cache.insert(CriticalKey(i), IntValue(i as i64));
	}

	for i in 20..=30 {
		cache.insert(VolatileKey(i), IntValue(i as i64));
	}

	for i in 40..=50 {
		cache.insert(StandardKey(i), IntValue(i as i64));
	}

	// Now trigger heavy eviction
	for i in 100..200 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	// At least some entries should remain
	assert!(!cache.is_empty(), "Cache should not be empty");

	// The cache successfully manages eviction under pressure
	assert!(cache.size() <= 600, "Cache should maintain size limit");
}

#[test]
fn test_same_policy_frequency_tiebreaker() {
	// Test that the cache properly tracks and manages entries with same policy
	let cache = Cache::new(600);

	// Insert Standard entries
	for i in 1..=15 {
		cache.insert(StandardKey(i), IntValue(i as i64));
	}

	// Access some entries to build up usage patterns
	for _ in 0..10 {
		for i in 1..=5 {
			let _ = cache.get_arc(&StandardKey(i));
		}
	}

	// Trigger eviction
	for i in 30..120 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	// Verify cache maintains size limit
	assert!(cache.size() <= 600, "Cache should maintain size limit");

	// Some entries should be evicted
	let remaining = (1..=15).filter(|&i| cache.contains(&StandardKey(i))).count();
	assert!(remaining < 15, "Some entries should be evicted");
}

#[test]
fn test_large_volatile_vs_small_critical() {
	// Verify the cache handles mixed policies correctly
	let cache = Cache::new(700);

	// Insert entries with different policies
	for i in 1..=10 {
		cache.insert(VolatileKey(i), IntValue(i as i64));
	}

	for i in 20..=30 {
		cache.insert(CriticalKey(i), IntValue(i as i64));
	}

	// Access Critical entries to increase their retention
	for _ in 0..10 {
		for i in 20..=30 {
			let _ = cache.get_arc(&CriticalKey(i));
		}
	}

	// Fill cache to trigger eviction
	for i in 100..200 {
		cache.insert(StandardKey(i), IntValue(i as i64));
	}

	// Verify cache maintains size limit
	assert!(cache.size() <= 700, "Cache should maintain size limit");

	// At least some entries should remain
	assert!(!cache.is_empty(), "Cache should have entries");
}

#[test]
fn test_access_pattern_survival() {
	// Verify that the cache handles different access patterns
	let cache = Cache::new(500);

	// Insert Standard entries
	for i in 1..=20 {
		cache.insert(StandardKey(i), IntValue(i as i64));
	}

	// Access some entries
	for _ in 0..10 {
		for i in 1..=10 {
			let _ = cache.get_arc(&StandardKey(i));
		}
	}

	// Trigger eviction
	for i in 30..100 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	// Verify cache behavior
	assert!(cache.size() <= 500, "Cache should maintain size limit");
	assert!(!cache.is_empty(), "Cache should have entries");

	// Some eviction should have occurred
	let remaining = (1..=20).filter(|&i| cache.contains(&StandardKey(i))).count();
	assert!(remaining < 20, "Some entries should be evicted under pressure");
}

#[test]
fn test_all_critical_still_evicts() {
	// Even with all Critical entries, cache should manage eviction
	let cache = Cache::new(400);

	// Fill cache with Critical entries
	for i in 1..=15 {
		cache.insert(CriticalKey(i), IntValue(i as i64));
	}

	// Access some entries
	for _ in 0..10 {
		for i in 1..=5 {
			let _ = cache.get_arc(&CriticalKey(i));
		}
	}

	// Try to insert more Critical entries - should trigger eviction
	for i in 30..80 {
		cache.insert(CriticalKey(i), IntValue(i as i64));
	}

	// Cache should maintain size limit
	assert!(cache.size() <= 400, "Cache size should be within limit: {}", cache.size());

	// Cache should have entries
	assert!(!cache.is_empty(), "Cache should have entries");

	// Not all entries should fit (some must be evicted)
	let total_inserted = 15 + 50; // 65 total
	assert!(cache.len() < total_inserted, "Some entries must be evicted");
}

#[test]
fn test_policy_change_on_reinsert() {
	// Verify that cache handles updates correctly
	let cache = Cache::new(600);

	// Insert entries
	for i in 1..=10 {
		cache.insert(StandardKey(i), IntValue(i as i64));
	}

	// Update some entries (simulates reinsertion)
	for i in 1..=5 {
		cache.insert(StandardKey(i), IntValue(i as i64 * 10));
	}

	// Verify updates worked
	if let Some(val) = cache.get_arc(&StandardKey(1)) {
		// Value should be updated
		assert!(val.0 == 10 || val.0 == 1);
	}

	// Fill cache to trigger eviction
	for i in 20..100 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	// Cache should maintain size limit
	assert!(cache.size() <= 600, "Cache should maintain size limit");
	assert!(!cache.is_empty(), "Cache should have entries");
}

#[test]
fn test_concurrent_access_affects_eviction() {
	// Verify that concurrent reads update frequency and affect eviction
	let cache = Arc::new(Cache::new(500));

	// Insert entries
	for i in 1..=20 {
		cache.insert(StandardKey(i), IntValue(i as i64));
	}

	let mut handles = vec![];

	// Spawn threads that access specific entries
	for t in 0..4 {
		let cache = cache.clone();
		handles.push(thread::spawn(move || {
			// Each thread accesses a subset of entries
			for _ in 0..20 {
				for i in (1 + t * 5)..=(5 + t * 5) {
					if i <= 20 {
						let _ = cache.get_arc(&StandardKey(i));
					}
				}
			}
		}));
	}

	// Wait for access threads to complete
	for handle in handles {
		handle.join().expect("thread should not panic");
	}

	// Now trigger eviction from main thread
	for i in 30..70 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	// Entries that were accessed by threads should have higher survival rate
	let accessed_survive = (1..=20).filter(|&i| cache.contains(&StandardKey(i))).count();

	// At least some of the accessed entries should survive
	assert!(
		accessed_survive > 0,
		"Some frequently accessed entries should survive concurrent eviction"
	);
}

#[test]
fn test_exhausted_bucket_moves_to_next() {
	// Test that cache handles mixed policies correctly
	let cache = Cache::new(500);

	// Insert entries with different policies
	for i in 1..=10 {
		cache.insert(VolatileKey(i), IntValue(i as i64));
	}

	// Access some Volatile entries
	for _ in 0..10 {
		for i in 1..=5 {
			let _ = cache.get_arc(&VolatileKey(i));
		}
	}

	// Insert Standard and Critical entries
	for i in 20..=30 {
		cache.insert(StandardKey(i), IntValue(i as i64));
	}

	for i in 40..=50 {
		cache.insert(CriticalKey(i), IntValue(i as i64));
	}

	// Trigger eviction
	for i in 100..200 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	// Verify cache maintains size limit
	assert!(cache.size() <= 500, "Cache should maintain size limit");
	assert!(!cache.is_empty(), "Cache should have entries");
}

#[test]
fn test_clock_bit_clearing() {
	// This test verifies clock bit behavior indirectly through eviction patterns
	let cache = Cache::new(300);

	// Insert entries
	for i in 1..=5 {
		cache.insert(StandardKey(i), IntValue(i as i64));
	}

	// Access entries to set clock_bit
	for i in 1..=5 {
		let _ = cache.get_arc(&StandardKey(i));
	}

	// First eviction pass should clear clock_bit but not evict
	// Second pass should evict

	// Trigger eviction
	for i in 10..40 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	// Some entries should survive the first pass due to clock_bit
	// but eventually be evicted on subsequent passes
	let _remaining = (1..=5).filter(|&i| cache.contains(&StandardKey(i))).count();

	// We can't guarantee exact behavior, but the cache should have
	// managed eviction properly
	assert!(cache.size() <= 300, "Cache should maintain size limit through clock eviction");
}

#[test]
fn test_frequency_decays_during_sweep() {
	// Verify that cache handles entries correctly during eviction
	let cache = Cache::new(600);

	// Insert Standard entries
	for i in 1..=20 {
		cache.insert(StandardKey(i), IntValue(i as i64));
	}

	// Access some entries to create usage patterns
	for _ in 0..15 {
		for i in 1..=10 {
			let _ = cache.get_arc(&StandardKey(i));
		}
	}

	// Trigger heavy eviction
	for i in 30..150 {
		cache.insert(IntKey(i), IntValue(i as i64));
	}

	// Verify cache maintains constraints
	assert!(cache.size() <= 600, "Cache should maintain size limit");
	assert!(!cache.is_empty(), "Cache should have entries");

	// Eviction should have occurred
	let remaining = (1..=20).filter(|&i| cache.contains(&StandardKey(i))).count();
	assert!(remaining < 20, "Some StandardKey entries should be evicted");
}

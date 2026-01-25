use std::sync::Arc;

use tokio;
/// Tests for async usage patterns.
use weighted_cache::{Cache, CacheKey, CacheValue};

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct AsyncKey(u64);

impl CacheKey for AsyncKey {
	type Value = AsyncValue;
}

#[derive(Clone, Debug, PartialEq)]
struct AsyncValue {
	data: String,
}

impl CacheValue for AsyncValue {
	fn deep_size(&self) -> usize {
		std::mem::size_of::<Self>() + self.data.capacity()
	}

	fn weight(&self) -> u64 {
		100
	}
}

#[tokio::test]
async fn test_get_arc_in_async() {
	let cache = Arc::new(Cache::new(10240));

	let key = AsyncKey(1);
	let value = AsyncValue {
		data: "async test".to_string(),
	};

	cache.insert(key.clone(), value.clone());

	// ✅ Correct: Use get_arc() and hold Arc across await
	if let Some(arc_value) = cache.get_arc(&key) {
		// Simulate async work
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		assert_eq!(*arc_value, value);
	}
}

#[tokio::test]
async fn test_get_clone_in_async() {
	let cache = Arc::new(Cache::new(10240));

	let key = AsyncKey(2);
	let value = AsyncValue {
		data: "clone test".to_string(),
	};

	cache.insert(key.clone(), value.clone());

	// ✅ Correct: Clone the value before await
	if let Some(cloned_value) = cache.get_clone(&key) {
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		assert_eq!(cloned_value, value);
	}
}

#[tokio::test]
async fn test_guard_scoped_correctly() {
	let cache = Arc::new(Cache::new(10240));

	let key = AsyncKey(3);
	let value = AsyncValue {
		data: "scoped test".to_string(),
	};

	cache.insert(key.clone(), value.clone());

	// ✅ Correct: Use guard in a tight scope, extract data, then await
	let extracted_data = {
		let guard = cache.get(&key).unwrap();
		guard.data.clone()
	}; // Guard is dropped here

	tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

	assert_eq!(extracted_data, "scoped test");
}

#[tokio::test]
async fn test_concurrent_async_tasks() {
	let cache = Arc::new(Cache::new(102400));

	// Pre-populate
	for i in 0..100 {
		cache.insert(
			AsyncKey(i),
			AsyncValue {
				data: format!("value-{}", i),
			},
		);
	}

	let mut handles = vec![];

	for task_id in 0..10 {
		let cache = cache.clone();
		handles.push(tokio::spawn(async move {
			for i in 0..100 {
				let key = AsyncKey((task_id * 100 + i) % 100);

				if let Some(value) = cache.get_arc(&key) {
					tokio::time::sleep(tokio::time::Duration::from_micros(1)).await;
					assert!(!value.data.is_empty());
				}
			}
		}));
	}

	for handle in handles {
		handle.await.unwrap();
	}
}

#[tokio::test]
async fn test_async_insert_and_get() {
	let cache = Arc::new(Cache::new(10240));

	let tasks: Vec<_> = (0..20)
		.map(|i| {
			let cache = cache.clone();
			tokio::spawn(async move {
				let key = AsyncKey(i);
				let value = AsyncValue {
					data: format!("async-{}", i),
				};

				cache.insert(key.clone(), value.clone());

				// Verify it was inserted
				tokio::time::sleep(tokio::time::Duration::from_micros(10)).await;

				if let Some(retrieved) = cache.get_arc(&key) {
					assert_eq!(*retrieved, value);
				}
			})
		})
		.collect();

	for task in tasks {
		task.await.unwrap();
	}
}

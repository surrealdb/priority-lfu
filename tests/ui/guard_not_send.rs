/// This test should fail to compile because Guard is !Send
/// and cannot be held across an await point in a Send future.

use weighted_cache::{Cache, CacheKey, CacheValue};
use std::sync::Arc;

#[derive(Hash, Eq, PartialEq, Clone)]
struct TestKey(u64);

impl CacheKey for TestKey {
    type Value = TestValue;
}

#[derive(Clone)]
struct TestValue(i32);

impl CacheValue for TestValue {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
    fn weight(&self) -> u64 {
        50
    }
}

async fn bad_async_pattern(cache: Arc<Cache>) {
    let key = TestKey(1);
    
    // Get a guard (holds a read lock)
    let guard = cache.get(&key).unwrap();
    
    // Try to hold guard across await - should fail to compile
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    
    println!("{:?}", *guard);
}

fn main() {
    let cache = Arc::new(Cache::new(1024));
    cache.insert(TestKey(1), TestValue(42));
    
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(bad_async_pattern(cache));
}

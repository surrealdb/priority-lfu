# Weighted Cache

A high-performance, concurrent, in-memory cache with **Clock-PRO** eviction policy and **weight-based** prioritization.

## Features

- 🚀 **High Performance**: Sub-microsecond reads with minimal overhead
- 📏 **Size-Bounded**: Memory limits in bytes, not item count
- ⚖️ **Weight-Based Eviction**: Prioritize important items regardless of size
- 🎯 **Clock-PRO Policy**: Scan-resistant eviction with high hit rates
- 🔒 **Thread-Safe**: Fine-grained sharding for low contention
- 🔄 **Async-Friendly**: Safe to use in async/await contexts
- 🎨 **Heterogeneous Storage**: Store different key/value types without enums

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
weighted-cache = "0.1"
```

Basic usage:

```rust
use weighted_cache::{Cache, CacheKey, DeepSizeOf};

// Define your key type
#[derive(Hash, Eq, PartialEq, Clone)]
struct UserId(u64);

// Define your value type
#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
struct UserData {
    name: String,
    score: i32,
}

// Implement required traits
impl CacheKey for UserId {
    type Value = UserData;

    fn weight(&self) -> u64 {
        // Higher weight = more resistant to eviction
        // Example: VIP users get higher weight based on ID
        if self.0 < 1000 { 200 } else { 50 }
    }
}

fn main() {
    // Create cache with 1GB capacity
    let cache = Cache::new(1024 * 1024 * 1024);

    // Insert values
    cache.insert(UserId(1), UserData {
        name: "Alice".to_string(),
        score: 1500,
    });

    // Retrieve values
    if let Some(user) = cache.get_arc(&UserId(1)) {
        println!("User: {}, Score: {}", user.name, user.score);
    }
}
```

## Async Usage

The cache is safe to use in async contexts:

```rust
use std::sync::Arc;

async fn process_user(cache: Arc<Cache>, user_id: UserId) {
    // ✅ Use get_arc() - returns Arc, releases lock immediately
    if let Some(user) = cache.get_arc(&user_id) {
        // Safe to hold Arc across await points
        expensive_async_operation(&user).await;
    }
}
```

**⚠️ Warning**: Don't hold `Guard` across `.await` points:

```rust
// ❌ BAD: Guard holds a lock
let guard = cache.get(&key).unwrap();
some_async_fn().await; // Will fail to compile in Send context

// ✅ GOOD: Extract data, drop guard, then await
let data = {
    let guard = cache.get(&key).unwrap();
    guard.clone()
}; // Guard dropped here
some_async_fn().await;
```

## Advanced Configuration

Use `CacheBuilder` for custom settings:

```rust
use weighted_cache::CacheBuilder;

let cache = CacheBuilder::new(1024 * 1024 * 512) // 512 MB
    .shards(128)                // More shards = less contention
    .hot_percent(0.85)          // 85% hot target (default: 90%)
    .build();
```

## How It Works

### Clock-PRO Algorithm

The cache uses **Clock-PRO**, a scan-resistant eviction policy that maintains three key data structures per shard:

1. **Hot List**: Frequently accessed entries (~90% of capacity by default)
2. **Cold List**: Recently inserted or demoted entries (~10% of capacity)
3. **Ghost List**: Recently evicted entry hashes for adaptive promotion

#### Eviction Process

When space is needed, the cache advances a "clock hand" through entries:

- **Cold entries** with reference bit = 0 → evicted (moved to ghost list)
- **Cold entries** with reference bit > 0 → promoted to hot (reference cleared)
- **Hot entries** with reference bit = 0 → demoted to cold
- **Hot entries** with reference bit > 0 → kept hot (reference cleared, moved to back)

Each access increments an entry's reference counter (atomic operation).

### Weight-Based Eviction

This implementation extends Clock-PRO with **weight-scaled reference counting**:

```rust
max_references = base_max + weight_bonus
// Low weight (1-99):      max_ref = 2
// Medium weight (100-999):  max_ref = 2-3
// High weight (1000+):      max_ref = 3-4
```

Higher-weight entries can accumulate more references before eviction, making them more resistant to being removed:

- **Low weight + low access** → evicted quickly
- **High weight + high access** → highly resistant to eviction

### Architecture

```
┌───────────────────────────────────────┐
│           Cache (Send + Sync)         │
│  ┌─────────────────────────────────┐  │
│  │ Global State (atomics)          │  │
│  │ - current_size: AtomicUsize     │  │
│  │ - entry_count: AtomicUsize      │  │
│  └─────────────────────────────────┘  │
│  ┌──────┐ ┌──────┐      ┌──────┐      │
│  │Shard0│ │Shard1│ ...  │ShardN│      │
│  │RwLock│ │RwLock│      │RwLock│      │
│  └──────┘ └──────┘      └──────┘      │
└───────────────────────────────────────┘
```

Each shard contains:
- **Hot List**: Frequently accessed entries (target: ~90% of shard capacity)
- **Cold List**: New/demoted entries (remaining capacity)
- **Ghost List**: Hash-only records of recently evicted entries for scan resistance

## Performance

- **Reads**: Sub-microsecond with atomic reference counting
- **Writes**: Optimized with adaptive eviction (stops early if pinned entries dominate)
- **Concurrency**: Default 64 shards for low contention
- **Memory**: ~128 bytes overhead per entry, ghost list tracks evicted hashes

## Thread Safety

The cache is `Send + Sync` and can be shared via `Arc`:

```rust
use std::sync::Arc;
use std::thread;

let cache = Arc::new(Cache::new(1024 * 1024));

let handles: Vec<_> = (0..4)
    .map(|_| {
        let cache = cache.clone();
        thread::spawn(move || {
            // Safe concurrent access
            cache.insert(key, value);
        })
    })
    .collect();

for handle in handles {
    handle.join().unwrap();
}
```

## Testing

Run tests:

```bash
cargo test
```

Run benchmarks:

```bash
cargo bench
```

## License

This project is licensed under the MIT License.

## Design

For detailed design documentation, see [design/spec.md](design/spec.md).

## References

- [CLOCK-Pro: An Effective Improvement of the CLOCK Replacement](https://www.usenix.org/legacy/events/usenix05/tech/general/full_papers/jiang/jiang.pdf) - Original paper
- [quick_cache](https://github.com/arthurprs/quick-cache) - Rust Clock-PRO implementation
- Size-based capacity management with byte-level tracking

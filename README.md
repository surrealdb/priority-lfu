# Weighted Cache

A high-performance, concurrent, in-memory cache with **W-TinyLFU** eviction policy and **weight-based** prioritization.

## Features

- 🚀 **High Performance**: Sub-microsecond reads with lock-free frequency tracking
- 📏 **Size-Bounded**: Memory limits in bytes, not item count
- ⚖️ **Weight-Based Eviction**: Prioritize important items regardless of size
- 🎯 **W-TinyLFU Policy**: Near-optimal hit rates with minimal overhead
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
use weighted_cache::{Cache, CacheKey, CacheValue};

// Define your key type
#[derive(Hash, Eq, PartialEq, Clone)]
struct UserId(u64);

// Define your value type
struct UserData {
    name: String,
    score: i32,
}

// Implement required traits
impl CacheKey for UserId {
    type Value = UserData;
}

impl CacheValue for UserData {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<Self>() + self.name.capacity()
    }

    fn weight(&self) -> u64 {
        // Higher weight = more resistant to eviction
        // Example: VIP users get higher weight
        if self.score > 1000 { 200 } else { 50 }
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
    .window_percent(0.02)       // 2% window (default: 1%)
    .protected_percent(0.75)    // 75% protected (default: 80%)
    .build();
```

## How It Works

### W-TinyLFU Algorithm

The cache uses **Window-TinyLFU**, which combines:

1. **Admission Window** (~1%): Captures recent items (LRU)
2. **TinyLFU Filter**: Probabilistic frequency counter for admission decisions
3. **SLRU Main Cache**: Segmented LRU (probationary + protected)

### Weight-Based Eviction

Standard W-TinyLFU evicts by frequency. This implementation adapts it for weights:

```
eviction_score = frequency_estimate / weight
```

Items with **lower scores** are evicted first:
- Low frequency, low weight → evicted first
- High frequency, high weight → evicted last

### Architecture

```
┌─────────────────────────────────────────┐
│           Cache (Send + Sync)           │
│  ┌─────────────────────────────────┐   │
│  │ Global State (atomics)          │   │
│  │ - current_size: AtomicUsize     │   │
│  │ - frequency_sketch: FreqSketch  │   │
│  └─────────────────────────────────┘   │
│  ┌──────┐ ┌──────┐      ┌──────┐      │
│  │Shard0│ │Shard1│ ...  │ShardN│      │
│  │RwLock│ │RwLock│      │RwLock│      │
│  └──────┘ └──────┘      └──────┘      │
└─────────────────────────────────────────┘
```

Each shard contains:
- **Window**: Recent insertions (~1% capacity)
- **Probationary**: New admissions (~20% of main)
- **Protected**: Frequently accessed (~80% of main)

## Performance

- **Reads**: Sub-microsecond with lock-free frequency tracking
- **Writes**: Optimized with deferred promotions and batched evictions
- **Concurrency**: Default 64 shards for low contention
- **Memory**: ~128 bytes overhead per entry + sketch (~0.1% of capacity)

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

- [TinyLFU: A Highly Efficient Cache Admission Policy](https://arxiv.org/abs/1512.00727)
- [Caffeine](https://github.com/ben-manes/caffeine) - Java implementation
- [Moka](https://github.com/moka-rs/moka) - Rust W-TinyLFU cache

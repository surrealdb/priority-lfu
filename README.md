# Weighted Cache

A high-performance, concurrent, in-memory cache with **weight-stratified clock** eviction policy and **policy-based** prioritization.

## Features

- 🚀 **High Performance**: Sub-microsecond reads with minimal overhead
- 📏 **Size-Bounded**: Memory limits in bytes, not item count
- ⚖️ **Policy-Based Eviction**: Prioritize important items with 4 eviction tiers
- 🎯 **Weight-Stratified Clock**: Predictable priority-based eviction with frequency tracking
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
use weighted_cache::{Cache, CacheKey, CachePolicy, DeepSizeOf};

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

    fn policy(&self) -> CachePolicy {
        // Different eviction priorities:
        // Critical - last to be evicted (metadata, schemas)
        // Durable - strong retention (active records)
        // Standard - normal eviction (default)
        // Volatile - first to be evicted (temp data)
        
        // Example: VIP users get higher priority based on ID
        if self.0 < 1000 { 
            CachePolicy::Critical 
        } else { 
            CachePolicy::Standard 
        }
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
    .build();
```

## How It Works

### Weight-Stratified Clock Algorithm

The cache uses a **weight-stratified clock** eviction policy that maintains four priority buckets per shard:

1. **Critical Bucket** (CachePolicy::Critical): Metadata, schemas, indexes - last to evict
2. **Standard Bucket** (CachePolicy::Standard): Normal cacheable data - default priority
3. **Volatile Bucket** (CachePolicy::Volatile): Temp data, intermediate results - first to evict

#### Eviction Process

When space is needed, the cache uses a clock sweep starting from the lowest priority bucket:

1. Start with **Volatile** bucket, advance clock hand
2. For each entry encountered:
   - If **clock_bit** is set → clear it and advance
   - If **clock_bit** is clear and **frequency** = 0 → **evict**
   - If **clock_bit** is clear and **frequency** > 0 → decrement frequency and advance
3. If bucket is empty, move to next higher priority bucket (Standard → Durable → Critical)

Each access sets the clock_bit and increments frequency (saturating at 255).

### Policy-Based Eviction

This design provides **predictable priority-based eviction**:

```rust
// Eviction order: Volatile → Standard → Durable → Critical
// Within each bucket: Clock sweep with LFU tie-breaking

CachePolicy::Volatile   // Evicted first
CachePolicy::Standard   // Normal eviction
CachePolicy::Critical   // Evicted last
```

Benefits:

- **Predictable**: Lower priority items always evicted before higher priority
- **Fast**: Only scans entries in the lowest-priority non-empty bucket
- **Simple**: No complex hot/cold promotion logic

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
- **Policy Buckets**: Four priority buckets (Critical, Durable, Standard, Volatile)
- **Clock Hands**: One per bucket for efficient clock sweep
- **Entry Map**: HashMap with pre-computed hashes for O(1) lookup

## Performance

- **Reads**: Sub-microsecond with atomic clock_bit and frequency updates
- **Writes**: Fast eviction by scanning only lowest priority bucket
- **Concurrency**: Default 64 shards for low contention
- **Memory**: ~96 bytes overhead per entry (no ghost list needed)

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

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
    if let Some(user) = cache.get_clone(&UserId(1)) {
        println!("User: {}, Score: {}", user.name, user.score);
    }
}
```

## Async Usage

The cache is safe to use in async contexts:

```rust,ignore
async fn process_user(cache: Arc<Cache>, user_id: UserId) {
    // ✅ Use get_clone() - releases lock immediately
    if let Some(user) = cache.get_clone(&user_id) {
        // Safe to hold Arc across await points
        expensive_async_operation(&user).await;
    }
}
```

**⚠️ Warning**: Don't hold `Guard` across `.await` points:

```rust,ignore
# let cache = weighted_cache::Cache::new(1024 * 1024);
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

```rust,ignore
use weighted_cache::CacheBuilder;

let cache = CacheBuilder::new(1024 * 1024 * 512) // 512 MB
    .shards(128)                // More shards = less contention
    .build();
```

## Metrics

The cache provides comprehensive performance metrics for monitoring and debugging.

**Note:** Metrics tracking is behind the `metrics` feature flag and not enabled by default to minimize overhead. To use metrics, add the feature to your `Cargo.toml`:

```toml
[dependencies]
weighted-cache = { version = "0.1", features = ["metrics"] }
```

Example usage:

```rust,ignore
use weighted_cache::Cache;

let cache = Cache::new(1024 * 1024);

// Perform some operations
cache.insert(key1, value1);
cache.insert(key2, value2);
cache.get_clone(&key1);  // hit
cache.get_clone(&key3);  // miss

// Get metrics snapshot
let metrics = cache.metrics();

// Access counters
println!("Cache hits: {}", metrics.hits);
println!("Cache misses: {}", metrics.misses);
println!("Inserts: {}", metrics.inserts);
println!("Updates: {}", metrics.updates);
println!("Evictions: {}", metrics.evictions);
println!("Removals: {}", metrics.removals);

// Computed metrics
println!("Hit rate: {:.2}%", metrics.hit_rate() * 100.0);
println!("Utilization: {:.2}%", metrics.utilization() * 100.0);
println!("Total accesses: {}", metrics.total_accesses());
println!("Total writes: {}", metrics.total_writes());

// Size and capacity
println!("Current size: {} bytes", metrics.current_size_bytes);
println!("Capacity: {} bytes", metrics.capacity_bytes);
println!("Entry count: {}", metrics.entry_count);
```

### Available Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `hits` | `u64` | Number of successful cache lookups |
| `misses` | `u64` | Number of failed cache lookups (key not found) |
| `inserts` | `u64` | Number of new entries inserted |
| `updates` | `u64` | Number of existing entries updated (key already existed) |
| `evictions` | `u64` | Number of entries evicted due to capacity constraints |
| `removals` | `u64` | Number of entries explicitly removed via `remove()` |
| `current_size_bytes` | `usize` | Current total size in bytes |
| `capacity_bytes` | `usize` | Maximum capacity in bytes |
| `entry_count` | `usize` | Current number of entries |

### Computed Metrics

The `CacheMetrics` struct provides helper methods for computed metrics:

- `hit_rate()` - Returns cache hit rate as a ratio (0.0 to 1.0)
- `utilization()` - Returns memory utilization as a ratio (0.0 to 1.0)
- `total_accesses()` - Returns total cache accesses (hits + misses)
- `total_writes()` - Returns total write operations (inserts + updates)

### Integration Example

Example of integrating with a monitoring system:

```rust,ignore
use std::time::Duration;
use std::sync::Arc;

async fn monitor_cache(cache: Arc<Cache>) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        
        let metrics = cache.metrics();
        
        // Send to your monitoring system (Prometheus, etc.)
        if metrics.hit_rate() < 0.5 {
            eprintln!("Warning: Cache hit rate is low: {:.2}%", 
                     metrics.hit_rate() * 100.0);
        }
        
        if metrics.utilization() > 0.9 {
            eprintln!("Warning: Cache utilization is high: {:.2}%",
                     metrics.utilization() * 100.0);
        }
    }
}
```

### Performance Impact

Metrics tracking adds overhead through atomic operations on every cache access:
- **6+ atomic operations per insert** (insert/update/eviction tracking)
- **2 atomic operations per get** (hit/miss tracking)
- Additional memory for storing 6 `AtomicU64` counters

If you don't need metrics, omit the `metrics` feature for maximum performance. The default build (without metrics) eliminates all tracking overhead.

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

```rust,ignore
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

```ignore
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

```rust,ignore
use std::sync::Arc;
use weighted_cache::Cache;

let cache = Arc::new(Cache::new(1024 * 1024));

let handles: Vec<_> = (0..4)
    .map(|_| {
        let cache = cache.clone();
        std::thread::spawn(move || {
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

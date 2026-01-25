# Weighted Cache Design Specification

## Overview

A high-performance, concurrent, in-memory cache with:
- **Size-bounded capacity** (bytes, not item count)
- **Weight-based eviction** (separate from size)
- **Heterogeneous storage** (multiple key/value types without a unified enum)
- **W-TinyLFU eviction policy** for near-optimal hit rates
- **Read-optimized concurrency** via fine-grained sharding

---

## Goals & Non-Goals

### Goals
- Sub-microsecond reads for cache hits
- Memory-bounded with configurable max size in bytes
- Eviction prioritizes low-weight items regardless of type
- Type-safe API without requiring a monolithic enum
- Thread-safe with minimal contention on reads

### Non-Goals
- Persistence / disk spillover
- TTL-based expiration (can be layered on top)
- Distributed caching
- Interior mutability of cached values

---

## Core API

### Traits

```rust
use std::hash::Hash;

/// Marker trait for cache keys. Associates a key type with its value type.
pub trait CacheKey: Hash + Eq + Clone + Send + Sync + 'static {
    type Value: CacheValue;
}

/// Trait for cacheable values.
pub trait CacheValue: Send + Sync + 'static {
    /// Memory footprint in bytes. Called once on insertion.
    /// Implement via `deepsize::DeepSizeOf` or manual calculation.
    fn deep_size(&self) -> usize;
    
    /// Eviction priority. Higher weight = more resistant to eviction.
    /// This is *not* the memory size—it's a logical priority.
    fn weight(&self) -> u64;
}
```

### Cache Interface

```rust
/// Thread-safe cache. Can be shared across threads via `Arc<Cache>`.
/// All methods are synchronous but safe to call from async contexts.
/// 
/// # Async Usage
/// ```rust
/// let cache = Arc::new(Cache::new(1024 * 1024));
/// 
/// // Share across tasks
/// let cache_clone = cache.clone();
/// tokio::spawn(async move {
///     // Use get_arc() or get_clone() in async contexts
///     // to avoid holding guards across await points
///     if let Some(value) = cache_clone.get_arc(&key) {
///         do_async_work(&value).await;
///     }
/// });
/// ```
impl Cache {
    /// Create a new cache with the given maximum size in bytes.
    pub fn new(max_size_bytes: usize) -> Self;
    
    /// Create with custom shard count (default: 64).
    pub fn with_shards(max_size_bytes: usize, shard_count: usize) -> Self;
    
    /// Insert a key-value pair. Evicts items if necessary.
    /// Returns the previous value if the key existed.
    pub fn insert<K: CacheKey>(&self, key: K, value: K::Value) -> Option<Arc<K::Value>>;
    
    /// Retrieve a value via guard. The guard holds a read lock on the shard.
    /// 
    /// # Warning
    /// Do NOT hold this guard across `.await` points. Use `get_arc()` instead
    /// for async contexts.
    pub fn get<K: CacheKey>(&self, key: &K) -> Option<Guard<'_, K::Value>>;
    
    /// Retrieve a value as `Arc<V>`. Safe to hold across `.await` points.
    /// This is the preferred method for async code.
    pub fn get_arc<K: CacheKey>(&self, key: &K) -> Option<Arc<K::Value>>;
    
    /// Retrieve a cloned value. Safe to hold across `.await` points.
    /// Requires `V: Clone`. Use `get_arc()` if clone is expensive.
    pub fn get_clone<K: CacheKey>(&self, key: &K) -> Option<K::Value>
    where
        K::Value: Clone;
    
    /// Remove a key from the cache.
    pub fn remove<K: CacheKey>(&self, key: &K) -> Option<Arc<K::Value>>;
    
    /// Check if a key exists without affecting frequency counters.
    pub fn contains<K: CacheKey>(&self, key: &K) -> bool;
    
    /// Current total size in bytes.
    pub fn size(&self) -> usize;
    
    /// Number of entries across all types.
    pub fn len(&self) -> usize;
    
    /// Clear all entries.
    pub fn clear(&self);
}

// ─────────────────────────────────────────────────────────────
// Thread Safety Guarantees
// ─────────────────────────────────────────────────────────────

// Cache is Send + Sync, can be wrapped in Arc and shared freely
unsafe impl Send for Cache {}
unsafe impl Sync for Cache {}

/// RAII guard for borrowed values. Holds a read lock on the shard.
/// 
/// **Intentionally `!Send`** to prevent holding across `.await` points,
/// which could cause deadlocks or excessive lock contention.
/// 
/// For async contexts, use `Cache::get_arc()` instead.
pub struct Guard<'a, V> {
    _lock: RwLockReadGuard<'a, Shard>,
    value: *const V,
    _marker: PhantomData<&'a V>,
}

// Guard is Sync (can be shared by reference) but NOT Send
unsafe impl<V: Sync> Sync for Guard<'_, V> {}
// Explicitly NOT implementing Send - this is intentional!
// impl<V> !Send for Guard<'_, V> {}  // Automatic due to RwLockReadGuard

impl<V> Deref for Guard<'_, V> {
    type Target = V;
    fn deref(&self) -> &V {
        unsafe { &*self.value }
    }
}
```

---

## Concurrency Model

### Thread Safety

The cache is designed for safe concurrent access from multiple threads, including async runtimes.

```rust
// Cache is Send + Sync
let cache: Arc<Cache> = Arc::new(Cache::new(1024 * 1024 * 512));

// Safe to share across threads
let cache_clone = cache.clone();
std::thread::spawn(move || {
    cache_clone.insert(key, value);
});

// Safe to share across async tasks  
let cache_clone = cache.clone();
tokio::spawn(async move {
    let value = cache_clone.get_arc(&key);
    do_async_work(value).await;
});
```

### Async Usage Patterns

**✅ Recommended: Use `get_arc()` in async code**

```rust
async fn process(cache: &Cache, key: &MyKey) {
    // get_arc() returns immediately, lock is released
    if let Some(value) = cache.get_arc(key) {
        // Safe to hold Arc across await
        expensive_async_operation(&value).await;
    }
}
```

**⚠️ Caution: Don't hold `Guard` across await points**

```rust
async fn bad_pattern(cache: &Cache, key: &MyKey) {
    // Guard holds a read lock!
    let guard = cache.get(key);  
    
    // ❌ BAD: Lock held across await, can cause deadlocks
    some_async_fn().await;
    
    println!("{:?}", *guard);
}
```

The `Guard` type is intentionally `!Send` to make this a compile error when the async block needs to be `Send` (which is most async runtimes).

**✅ If you need the guard, scope it tightly**

```rust
async fn ok_pattern(cache: &Cache, key: &MyKey) {
    let data = {
        // Guard is dropped at end of block
        let guard = cache.get(key);
        guard.map(|g| g.some_field.clone())
    };
    
    // Now safe to await
    some_async_fn().await;
    
    if let Some(d) = data {
        // use d
    }
}
```

### Internal Synchronization

| Component | Synchronization | Rationale |
|-----------|----------------|-----------|
| Shards | `parking_lot::RwLock` | Fast, no poisoning, read-preferring |
| Global size | `AtomicUsize` | Lock-free updates |
| Entry segment | `AtomicU8` | Lock-free promotion marking |
| Frequency sketch | `AtomicU64` (packed counters) | Lock-free increments |
| Promotion buffer | `crossbeam::SegQueue` | Lock-free MPMC queue |

### Why Not `tokio::sync::RwLock`?

While `tokio::sync::RwLock` is async-aware, it has overhead that's unnecessary here:

1. **Cache operations are fast** - Sub-microsecond, no I/O, no reason to yield
2. **Lock hold times are minimal** - Just HashMap lookup + Arc clone
3. **Blocking is acceptable** - `parking_lot` blocking is efficient and brief
4. **Simpler API** - Works identically in sync and async code

The `get_arc()` pattern gives you async-friendly semantics without async lock complexity.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         Cache (Send + Sync)                     │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  Global State (lock-free atomics)                         │  │
│  │  - current_size: AtomicUsize                              │  │
│  │  - entry_count: AtomicUsize                               │  │
│  └───────────────────────────────────────────────────────────┘  │
│                                                                 │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  Frequency Sketch (AtomicU64 array, lock-free)            │  │
│  │  - 4 rows of 4-bit saturating counters                    │  │
│  │  - Periodic halving via compare-and-swap                  │  │
│  └───────────────────────────────────────────────────────────┘  │
│                                                                 │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  Promotion Buffer (crossbeam::SegQueue, lock-free MPMC)   │  │
│  │  - Deferred promotions from probationary → protected      │  │
│  │  - Drained periodically or on write operations            │  │
│  └───────────────────────────────────────────────────────────┘  │
│                                                                 │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐       ┌─────────┐         │
│  │ Shard 0 │ │ Shard 1 │ │ Shard 2 │  ...  │ Shard N │         │
│  │ RwLock  │ │ RwLock  │ │ RwLock  │       │ RwLock  │         │
│  │(parking │ │(parking │ │(parking │       │(parking │         │
│  │  _lot)  │ │  _lot)  │ │  _lot)  │       │  _lot)  │         │
│  └────┬────┘ └────┬────┘ └────┬────┘       └────┬────┘         │
│       │           │           │                 │               │
│       ▼           ▼           ▼                 ▼               │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐       ┌─────────┐         │
│  │ HashMap │ │ HashMap │ │ HashMap │       │ HashMap │         │
│  │ erased  │ │ erased  │ │ erased  │       │ erased  │         │
│  │ Entry:  │ │ Entry:  │ │ Entry:  │       │ Entry:  │         │
│  │ Arc<V>  │ │ Arc<V>  │ │ Arc<V>  │       │ Arc<V>  │         │
│  └─────────┘ └─────────┘ └─────────┘       └─────────┘         │
│                                                                 │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  Per-Shard: Window + SLRU segments (IndexMap for order)   │  │
│  │  - Window: ~1% capacity, recent inserts                   │  │
│  │  - Probationary: ~20% of main, new admissions             │  │
│  │  - Protected: ~80% of main, promoted on re-access         │  │
│  └───────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘

Data Flow:
═══════════

Insert:  key,value ──▶ Shard[hash] ──▶ Window ──▶ [admit?] ──▶ Probationary
                                                     │
                                                     ▼ (rejected)
                                                   Evict

Read:    key ──▶ Shard[hash].read_lock() ──▶ Arc::clone(value) ──▶ unlock
                                │
                                └──▶ frequency_sketch.increment() (lock-free)
                                └──▶ promotion_buffer.push() (if probationary)

Async:   get_arc() returns Arc<V>, lock released immediately
         Arc can safely cross .await points
```

---

## Type Erasure Strategy

### Erased Key

```rust
use std::any::TypeId;

#[derive(Clone)]
struct ErasedKey {
    /// TypeId of the concrete key type K
    type_id: TypeId,
    /// Pre-computed hash of (TypeId, K)
    hash: u64,
    /// The actual key, boxed and type-erased
    data: Arc<dyn Any + Send + Sync>,
}

impl ErasedKey {
    fn new<K: CacheKey>(key: &K) -> Self {
        let type_id = TypeId::of::<K>();
        let hash = compute_hash(type_id, key);
        Self {
            type_id,
            hash,
            data: Arc::new(key.clone()),
        }
    }
    
    fn downcast_ref<K: 'static>(&self) -> Option<&K> {
        self.data.downcast_ref()
    }
}

impl Hash for ErasedKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);  // Use pre-computed hash
    }
}

impl PartialEq for ErasedKey {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.type_id == other.type_id
        // Note: hash collision requires deep equality check
        // See implementation section for full details
    }
}
```

### Erased Entry

```rust
struct Entry {
    /// Type-erased value wrapped in Arc for cheap sharing across threads.
    /// Enables `get_arc()` to return without cloning the underlying value.
    value: Arc<dyn Any + Send + Sync>,
    /// Cached deep_size() result (immutable after creation)
    size: usize,
    /// Cached weight() result (immutable after creation)
    weight: u64,
    /// TypeId for safe downcasting
    type_id: TypeId,
    /// W-TinyLFU segment location
    segment: AtomicU8,  // Lock-free segment updates
}

impl Entry {
    fn new<V: CacheValue>(value: V) -> Self {
        Self {
            size: value.deep_size(),
            weight: value.weight(),
            type_id: TypeId::of::<V>(),
            value: Arc::new(value),
            segment: AtomicU8::new(Segment::Window as u8),
        }
    }
    
    /// Clone the Arc (cheap reference count bump, no data copy)
    fn value_arc<V: 'static>(&self) -> Option<Arc<V>> {
        // Clone the Arc, then downcast
        let arc_any = self.value.clone();
        Arc::downcast::<V>(arc_any).ok()
    }
}

#[derive(Clone, Copy)]
enum Segment {
    Window,
    Probationary,
    Protected,
}
```

---

## W-TinyLFU Algorithm

W-TinyLFU (Window Tiny Least Frequently Used) combines:
1. **Admission window**: Small LRU that captures recency
2. **TinyLFU filter**: Probabilistic frequency counter for admission decisions
3. **SLRU main cache**: Segmented LRU for frequency-aware eviction

### Why W-TinyLFU?

| Policy | Hit Rate | Memory Overhead | Scan Resistance |
|--------|----------|-----------------|-----------------|
| LRU | Baseline | Low | Poor |
| LFU | +5-10% | High | Good |
| TinyLFU | +5-10% | Very Low | Good |
| **W-TinyLFU** | **+10-15%** | **Very Low** | **Excellent** |

### Adaptation for Weight-Based Eviction

Standard W-TinyLFU evicts by frequency. We modify it:

```
eviction_score = frequency_estimate / weight
```

Lower score = higher eviction priority. This means:
- Low frequency, low weight → evicted first
- High frequency, high weight → evicted last

### Components

#### 1. Frequency Sketch (Count-Min Sketch)

```rust
struct FrequencySketch {
    /// 4 rows of counters (4-bit saturating counters packed into u64)
    table: Vec<AtomicU64>,
    /// Number of counter slots per row
    mask: usize,
    /// Seeds for hash functions (one per row)
    seeds: [u64; 4],
    /// Total increments (for periodic halving)
    additions: AtomicUsize,
    /// Sample size before reset
    sample_size: usize,
}

impl FrequencySketch {
    /// Increment frequency for a key's hash.
    fn increment(&self, hash: u64);
    
    /// Estimate frequency (returns 0-15).
    fn frequency(&self, hash: u64) -> u8;
    
    /// Halve all counters (age out old frequencies).
    fn reset(&self);
}
```

**Sizing**: 8 bytes per counter slot. For 1M unique items, use ~256KB (32K slots × 4 rows × 2 bytes effective).

#### 2. Admission Window

A small LRU holding recently inserted items (~1% of max_size).

```rust
struct Window {
    /// Ordered map: insertion order for LRU
    entries: IndexMap<ErasedKey, ()>,
    /// Current size in bytes
    size: usize,
    /// Max size (1% of total cache capacity)
    max_size: usize,
}
```

#### 3. SLRU Main Cache

Two segments:
- **Probationary** (20%): New admissions from window
- **Protected** (80%): Promoted on second access

```rust
struct SegmentedLRU {
    probationary: IndexMap<ErasedKey, ()>,
    probationary_size: usize,
    probationary_max: usize,  // 20% of main cache
    
    protected: IndexMap<ErasedKey, ()>,
    protected_size: usize,
    protected_max: usize,  // 80% of main cache
}
```

---

## Algorithms

### Insert

```
fn insert(key K, value V):
    erased_key = ErasedKey::new(&key)
    shard = get_shard(erased_key.hash)
    entry = Entry::new(value)  // Caches size and weight
    
    // 1. Check if already exists
    shard.write_lock()
    if shard.contains(&erased_key):
        old = shard.replace(erased_key, entry)
        update_global_size(entry.size - old.size)
        return Some(old.value)
    
    // 2. Check if space available
    new_size = global_size.load() + entry.size
    while new_size > max_size:
        evict_one()  // See eviction algorithm
        new_size = global_size.load() + entry.size
    
    // 3. Insert into window
    shard.insert(erased_key, entry, Segment::Window)
    window.push_back(erased_key)
    global_size.fetch_add(entry.size)
    frequency_sketch.increment(erased_key.hash)
    
    // 4. If window full, try to admit to main cache
    if window.size > window.max_size:
        admit_from_window()
    
    return None
```

### Admit from Window

```
fn admit_from_window():
    // Pop oldest from window
    candidate = window.pop_front()
    
    if main_cache.size + candidate.size <= main_cache.max_size:
        // Space available, admit directly
        move candidate to probationary
        return
    
    // Find eviction victim in probationary segment
    victim = probationary.peek_oldest()
    
    // TinyLFU admission: compare frequencies adjusted by weight
    candidate_score = frequency(candidate.hash) / candidate.weight
    victim_score = frequency(victim.hash) / victim.weight
    
    if candidate_score > victim_score:
        evict(victim)
        move candidate to probationary
    else:
        evict(candidate)  // Candidate rejected
```

### Get (Read)

```
fn get<K>(key: &K) -> Option<Guard<V>>:
    erased_key = ErasedKey::new(key)
    shard = get_shard(erased_key.hash)
    
    // 1. Read lock for lookup (guard holds this lock)
    lock = shard.read_lock()
    entry = shard.get(&erased_key)?
    
    // 2. Increment frequency (lock-free)
    frequency_sketch.increment(erased_key.hash)
    
    // 3. Promotion check (deferred to avoid write lock)
    if entry.segment.load() == Probationary:
        promotion_buffer.push(erased_key)  // Lock-free queue
    
    return Some(Guard::new(lock, entry.value_ptr()))

fn get_arc<K>(key: &K) -> Option<Arc<V>>:
    erased_key = ErasedKey::new(key)
    shard = get_shard(erased_key.hash)
    
    // 1. Short-lived read lock
    {
        lock = shard.read_lock()
        entry = shard.get(&erased_key)?
        arc = entry.value_arc::<V>()?  // Clone Arc (cheap refcount bump)
    }  // Lock released here
    
    // 2. Increment frequency (lock-free, outside lock)
    frequency_sketch.increment(erased_key.hash)
    
    // 3. Promotion check
    if entry.segment.load() == Probationary:
        promotion_buffer.push(erased_key)
    
    return Some(arc)  // Arc can now be held across await points
```

**Optimization**: Defer promotions to a background task or batch them to avoid write locks on hot reads.

### Eviction

```
fn evict_one():
    // Try probationary first (lower value items)
    if let Some(victim) = find_lowest_score(probationary):
        remove(victim)
        return
    
    // Then protected
    if let Some(victim) = find_lowest_score(protected):
        remove(victim)
        return
    
    // Finally window
    if let Some(victim) = find_lowest_score(window):
        remove(victim)
        return

fn find_lowest_score(segment) -> Option<ErasedKey>:
    // Score = frequency / weight (lower = more evictable)
    // Sample a subset for performance (e.g., 5 random items)
    best = None
    best_score = f64::MAX
    
    for _ in 0..SAMPLE_SIZE:
        candidate = segment.random_entry()
        freq = frequency_sketch.frequency(candidate.hash)
        score = (freq as f64) / (candidate.weight as f64)
        if score < best_score:
            best = Some(candidate)
            best_score = score
    
    return best
```

---

## Sharding Strategy

### Shard Count

For read-heavy workloads: `num_shards = num_cpus * 16` (typically 64-256).

More shards = less contention, but more memory overhead and more complex eviction.

### Shard Selection

```rust
fn get_shard(&self, hash: u64) -> &RwLock<Shard> {
    let index = (hash as usize) & (self.shards.len() - 1);
    &self.shards[index]
}
```

### Cross-Shard Eviction

Eviction must consider all shards. Options:

1. **Global eviction lock** (simple, contention risk)
2. **Probabilistic sampling** (sample 1-2 items per shard, evict lowest)
3. **Per-shard budgets** (approximate, can have imbalance)

**Recommendation**: Probabilistic sampling with global size tracking.

```rust
fn evict_one(&self) {
    let mut candidates = Vec::with_capacity(self.shards.len());
    
    // Sample one candidate from each shard
    for shard in &self.shards {
        if let Ok(guard) = shard.try_read() {
            if let Some(candidate) = guard.sample_eviction_candidate() {
                candidates.push(candidate);
            }
        }
    }
    
    // Find global minimum score
    let victim = candidates.into_iter()
        .min_by(|a, b| a.score().partial_cmp(&b.score()).unwrap())?;
    
    // Evict (requires write lock on victim's shard)
    self.remove_internal(&victim.key);
}
```

---

## Memory Layout

### Entry Overhead

Per entry:
- `ErasedKey`: ~48 bytes (TypeId + hash + Arc)
- `Entry`: ~48 bytes (Box + size + weight + TypeId + segment)
- HashMap overhead: ~32 bytes per entry
- **Total overhead**: ~128 bytes per entry + key size + value size

### Frequency Sketch Sizing

```
sketch_size = max(cache_size / 1024, 256KB)
```

For 1GB cache: 1MB sketch (negligible).

---

## Implementation Phases

### Phase 1: Core Types (1-2 days)
- [ ] Define `CacheKey` and `CacheValue` traits
- [ ] Implement `ErasedKey` with hash and equality
- [ ] Implement `Entry` with size/weight caching
- [ ] Basic single-threaded HashMap wrapper
- [ ] Unit tests for type erasure

### Phase 2: Frequency Sketch (1 day)
- [ ] Implement Count-Min Sketch with 4-bit counters
- [ ] Atomic increment and read operations
- [ ] Periodic halving (reset) logic
- [ ] Benchmarks for throughput

### Phase 3: SLRU Structure (2 days)
- [ ] Window segment with LRU ordering
- [ ] Probationary and Protected segments
- [ ] Promotion logic (probationary → protected)
- [ ] Size tracking per segment

### Phase 4: W-TinyLFU Eviction (2 days)
- [ ] Admission decision (window → main)
- [ ] Weight-adjusted scoring
- [ ] Eviction with sampling
- [ ] Integration tests

### Phase 5: Sharding & Concurrency (2 days)
- [ ] Shard structure with `parking_lot::RwLock`
- [ ] Global atomic size tracking
- [ ] Cross-shard eviction sampling
- [ ] Lock-free promotion buffer (`crossbeam::SegQueue`)
- [ ] Implement `get_arc()` for async-friendly access
- [ ] Concurrent access tests
- [ ] Verify `Send + Sync` bounds
- [ ] Test `Guard` is `!Send` (compile-fail test)

### Phase 6: API & Ergonomics (1 day)
- [ ] Public `Cache` API
- [ ] `Guard` type for zero-copy reads
- [ ] Builder pattern for configuration
- [ ] Documentation

### Phase 7: Optimization (2-3 days)
- [ ] Batched promotions
- [ ] Lock-free frequency increments
- [ ] Benchmarking suite
- [ ] Profiling and tuning

---

## Testing Strategy

### Unit Tests
- Type erasure round-trips
- Frequency sketch accuracy
- SLRU ordering invariants
- Weight-based eviction ordering

### Integration Tests
- Mixed type insertions and retrievals
- Concurrent read/write stress tests
- Memory bound enforcement
- Eviction under pressure

### Property-Based Tests (proptest)
- Arbitrary insert/get/remove sequences
- Size accounting invariants
- No memory leaks under churn

### Benchmarks
- Single-threaded throughput (vs quick_cache, moka)
- Concurrent throughput at various reader/writer ratios
- Hit rate on synthetic workloads (Zipf distribution)
- Memory overhead per entry

---

## Configuration Options

```rust
pub struct CacheBuilder {
    max_size: usize,
    shard_count: Option<usize>,         // Default: num_cpus * 16
    window_percent: f32,                 // Default: 1%
    protected_percent: f32,              // Default: 80% of main
    sketch_size: Option<usize>,          // Default: auto
}

impl CacheBuilder {
    pub fn new(max_size_bytes: usize) -> Self;
    pub fn shards(self, count: usize) -> Self;
    pub fn window_percent(self, percent: f32) -> Self;
    pub fn protected_percent(self, percent: f32) -> Self;
    pub fn build(self) -> Cache;
}
```

---

## Future Enhancements

1. **TTL support**: Optional per-entry expiration
2. **Async API**: `get_async`, `insert_async` with tokio
3. **Metrics**: Hit rate, eviction count, size distribution
4. **Persistence**: Optional disk backup/restore
5. **Entry pinning**: Prevent eviction of critical entries

---

## Dependencies

```toml
[dependencies]
parking_lot = "0.12"       # Fast RwLock, no poisoning
crossbeam-queue = "0.3"    # Lock-free promotion buffer
indexmap = "2"             # Ordered map for LRU segments
ahash = "0.8"              # Fast hashing (or use foldhash)

[dev-dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
criterion = "0.5"          # Benchmarks
proptest = "1"             # Property-based testing
trybuild = "1"             # Compile-fail tests for !Send guard
```

### Optional Dependencies

```toml
[dependencies]
deepsize = { version = "0.2", optional = true }  # Auto deep_size impl

[features]
default = []
deepsize = ["dep:deepsize"]
```

---

## References

- [TinyLFU: A Highly Efficient Cache Admission Policy](https://arxiv.org/abs/1512.00727)
- [quick_cache source](https://github.com/arthurprs/quick_cache)
- [moka cache](https://github.com/moka-rs/moka) (Rust W-TinyLFU impl)
- [Caffeine](https://github.com/ben-manes/caffeine) (Java, canonical W-TinyLFU)

# Benchmarks

This directory contains comprehensive benchmarks for the `priority-lfu` cache implementation.

## Running Benchmarks

Run all benchmarks:
```bash
cargo bench --package priority-lfu
```

Run specific benchmark groups:
```bash
# Basic operations
cargo bench --package priority-lfu -- insert
cargo bench --package priority-lfu -- get_hit

# Borrowed key lookups
cargo bench --package priority-lfu -- borrowed_key

# Comparison with quick_cache
cargo bench --package priority-lfu -- comparison
```

## Benchmark Categories

### 1. Basic Operations

- **`bench_insert`**: Measures insertion performance with varying numbers of entries
- **`bench_get_hit`**: Measures successful lookup performance
- **`bench_get_vs_get_clone`**: Compares `get()` (returns Guard) vs `get_clone()` (clones value)
- **`bench_mixed_workload`**: Simulates realistic 80% read / 20% write workload
- **`bench_concurrent_reads`**: Tests concurrent read performance with 4 threads
- **`bench_eviction_pressure`**: Measures performance under memory pressure with active eviction
- **`bench_hit_rate_zipf`**: Tests performance with Zipf distribution (realistic access patterns)

### 2. Borrowed Key Lookups

These benchmarks demonstrate the **zero-allocation** performance benefits of the `CacheKeyLookup` trait when using borrowed keys instead of owned keys.

**Use case**: When you have string-based keys like `DbCacheKey(String, String)` and need to look them up using `&str` references without allocating owned strings.

#### Benchmarks:

- **`bench_owned_vs_borrowed_get`**: Compares `get(&owned_key)` vs `get_by(&borrowed_key)`
  - Owned approach: Creates `StringKey(String, String)` for each lookup (allocates)
  - Borrowed approach: Uses `StringKeyRef(&str, &str)` (zero allocation)

- **`bench_owned_vs_borrowed_get_clone`**: Compares `get_clone(&owned_key)` vs `get_clone_by(&borrowed_key)`
  - Measures cloned value retrieval with owned vs borrowed keys

- **`bench_owned_vs_borrowed_contains`**: Compares `contains(&owned_key)` vs `contains_by(&borrowed_key)`
  - Shows allocation overhead even for simple existence checks

- **`bench_borrowed_key_realistic_workload`**: Simulates realistic database cache usage
  - 80% reads (lookups) + 20% writes (inserts)
  - Demonstrates performance benefits in production-like scenarios

- **`bench_borrowed_key_different_sizes`**: Tests performance with varying key sizes
  - Key sizes: 10, 50, 100, 200 characters
  - Shows how allocation overhead scales with key size

#### Performance Expectations:

The borrowed key approach provides the most benefit when:
1. You already have string slices available (e.g., from parsing, network buffers, or external data)
2. Keys are frequently looked up (read-heavy workloads)
3. Key strings are large (more allocation overhead to avoid)

**Note**: In the benchmarks where we create formatted strings before lookup, you may not see significant differences because the string allocation cost dominates. The real-world benefit comes when you already have `&str` references available (e.g., from a request parser, configuration file, or database query).

### 3. Comparison Benchmarks

These benchmarks compare `priority-lfu` against `quick_cache` to validate performance characteristics:

- **`bench_comparison_insert`**: Insert performance comparison
- **`bench_comparison_get_hit`**: Lookup performance comparison
- **`bench_comparison_mixed_workload`**: Mixed read/write performance
- **`bench_comparison_concurrent_reads`**: Multi-threaded read performance
- **`bench_comparison_eviction_pressure`**: Eviction algorithm performance
- **`bench_comparison_zipf_distribution`**: Performance with realistic access patterns

## Example Results

To see a comparison between owned and borrowed key lookups:

```bash
cargo bench --package priority-lfu -- "borrowed_key/get"
```

Example output:
```
borrowed_key/get/owned_key    time:   [112.80 µs 114.21 µs 115.85 µs]
borrowed_key/get/borrowed_key time:   [133.93 µs 135.19 µs 137.35 µs]
```

**Important**: The above example creates new strings before lookup in both cases. For true zero-allocation benefits, use the borrowed key approach when you already have `&str` slices available from your data source.

## Interpreting Results

- Lower time values are better
- Look for consistent performance across different workload patterns
- Pay attention to outliers and variance
- Compare against `quick_cache` benchmarks to validate competitive performance
- For borrowed key benchmarks, focus on scenarios where you already have string slices available

## Adding New Benchmarks

1. Add your benchmark function to `cache_benchmark.rs`
2. Follow the existing naming convention: `bench_<category>_<operation>`
3. Use `black_box()` to prevent compiler optimizations from eliminating code
4. Include the benchmark in the `criterion_group!` macro at the end of the file
5. Document what the benchmark measures in this README

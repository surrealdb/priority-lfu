use std::hint::black_box;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use priority_lfu::{Cache, CacheKey, CacheKeyLookup, CachePolicy, DeepSizeOf};
use quick_cache::sync::Cache as QuickCache;

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct BenchKey(u64);

impl CacheKey for BenchKey {
	type Value = BenchValue;

	fn policy(&self) -> CachePolicy {
		CachePolicy::Standard
	}
}

#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
struct BenchValue {
	data: Vec<u8>,
}

fn bench_insert(c: &mut Criterion) {
	let mut group = c.benchmark_group("insert");

	for size in [100, 1000, 10000] {
		group.throughput(Throughput::Elements(size));
		group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
			b.iter(|| {
				let cache = Cache::new(1024 * 1024);
				for i in 0..size {
					let key = BenchKey(i);
					let value = BenchValue {
						data: vec![0u8; 64],
					};
					cache.insert(black_box(key), black_box(value));
				}
			});
		});
	}

	group.finish();
}

fn bench_get_hit(c: &mut Criterion) {
	let cache = Arc::new(Cache::new(1024 * 1024));

	// Pre-populate cache
	for i in 0..1000 {
		cache.insert(
			BenchKey(i),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	c.bench_function("get_hit", |b| {
		b.iter(|| {
			for i in 0..1000 {
				let key = BenchKey(black_box(i));
				black_box(cache.get_clone(&key));
			}
		});
	});
}

fn bench_get_vs_get_clone(c: &mut Criterion) {
	let cache = Arc::new(Cache::new(1024 * 1024));

	// Pre-populate
	for i in 0..100 {
		cache.insert(
			BenchKey(i),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	let mut group = c.benchmark_group("get_methods");

	group.bench_function("get", |b| {
		b.iter(|| {
			for i in 0..100 {
				black_box(cache.get(&BenchKey(black_box(i))));
			}
		});
	});

	group.bench_function("get_clone", |b| {
		b.iter(|| {
			for i in 0..100 {
				black_box(cache.get_clone(&BenchKey(black_box(i))));
			}
		});
	});

	group.finish();
}

fn bench_mixed_workload(c: &mut Criterion) {
	let cache = Arc::new(Cache::new(1024 * 1024));

	// Pre-populate
	for i in 0..500 {
		cache.insert(
			BenchKey(i),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	c.bench_function("mixed_80_20", |b| {
		b.iter(|| {
			for i in 0..100 {
				if i % 5 == 0 {
					// 20% writes
					cache.insert(
						BenchKey(black_box(i)),
						BenchValue {
							data: vec![0u8; 64],
						},
					);
				} else {
					// 80% reads
					black_box(cache.get_clone(&BenchKey(black_box(i % 500))));
				}
			}
		});
	});
}

fn bench_concurrent_reads(c: &mut Criterion) {
	use std::thread;

	let cache = Arc::new(Cache::new(1024 * 1024));

	// Pre-populate
	for i in 0..1000 {
		cache.insert(
			BenchKey(i),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	c.bench_function("concurrent_reads_4_threads", |b| {
		b.iter(|| {
			let mut handles = vec![];

			for _ in 0..4 {
				let cache = cache.clone();
				handles.push(thread::spawn(move || {
					for i in 0..250 {
						black_box(cache.get_clone(&BenchKey(black_box(i))));
					}
				}));
			}

			for handle in handles {
				handle.join().expect("thread should not panic");
			}
		});
	});
}

fn bench_eviction_pressure(c: &mut Criterion) {
	c.bench_function("eviction_pressure", |b| {
		b.iter(|| {
			let cache = Cache::new(10240); // Small cache to trigger eviction

			// Insert many items, forcing eviction
			for i in 0..1000 {
				cache.insert(
					BenchKey(black_box(i)),
					BenchValue {
						data: vec![0u8; 100],
					},
				);
			}
		});
	});
}

fn bench_hit_rate_zipf(c: &mut Criterion) {
	let cache = Arc::new(Cache::new(1024 * 1024));

	// Simulate Zipf distribution: some keys are accessed much more frequently
	let zipf_keys: Vec<u64> = (0..100)
		.flat_map(|i| {
			let freq = 100 / (i + 1); // First key appears 100 times, second 50 times, etc.
			vec![i; freq as usize]
		})
		.collect();

	c.bench_function("zipf_distribution", |b| {
		b.iter(|| {
			for &key_id in &zipf_keys {
				let key = BenchKey(black_box(key_id));
				match cache.get_clone(&key) {
					Some(val) => {
						black_box(val);
					}
					None => {
						cache.insert(
							key,
							BenchValue {
								data: vec![0u8; 64],
							},
						);
					}
				}
			}
		});
	});
}

// ============================================================================
// Comparison Benchmarks: priority-lfu vs quick_cache
// ============================================================================

fn bench_comparison_insert(c: &mut Criterion) {
	let mut group = c.benchmark_group("comparison/insert");

	// Use same item capacity for fair comparison
	const CACHE_CAPACITY: usize = 20000;

	for size in [100, 1000, 10000] {
		group.throughput(Throughput::Elements(size));

		group.bench_with_input(BenchmarkId::new("priority_lfu", size), &size, |b, &size| {
			b.iter(|| {
				// Estimate ~100 bytes per entry (64 byte value + key + Arc overhead)
				let cache = Cache::new(CACHE_CAPACITY * 100);
				for i in 0..size {
					let key = BenchKey(i);
					let value = BenchValue {
						data: vec![0u8; 64],
					};
					cache.insert(black_box(key), black_box(value));
				}
			});
		});

		group.bench_with_input(BenchmarkId::new("quick_cache", size), &size, |b, &size| {
			b.iter(|| {
				let cache = QuickCache::new(CACHE_CAPACITY);
				for i in 0..size {
					let key = i;
					let value = vec![0u8; 64];
					cache.insert(black_box(key), black_box(value));
				}
			});
		});
	}

	group.finish();
}

fn bench_comparison_get_hit(c: &mut Criterion) {
	let mut group = c.benchmark_group("comparison/get_hit");

	// Use same item capacity for fair comparison
	const NUM_ITEMS: u64 = 1000;
	const CACHE_CAPACITY: usize = 2000;

	// priority-lfu setup (~100 bytes per entry)
	let lfu_cache = Arc::new(Cache::new(CACHE_CAPACITY * 100));
	for i in 0..NUM_ITEMS {
		lfu_cache.insert(
			BenchKey(i),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	// quick_cache setup
	let quick_cache = Arc::new(QuickCache::new(CACHE_CAPACITY));
	for i in 0..NUM_ITEMS {
		quick_cache.insert(i, vec![0u8; 64]);
	}

	group.bench_function("priority_lfu", |b| {
		b.iter(|| {
			for i in 0..NUM_ITEMS {
				let key = BenchKey(black_box(i));
				black_box(lfu_cache.get(&key));
			}
		});
	});

	group.bench_function("quick_cache", |b| {
		b.iter(|| {
			for i in 0..NUM_ITEMS {
				black_box(quick_cache.get(&black_box(i)));
			}
		});
	});

	group.finish();
}

fn bench_comparison_mixed_workload(c: &mut Criterion) {
	let mut group = c.benchmark_group("comparison/mixed_80_20");

	// Use same item capacity for fair comparison
	const NUM_ITEMS: u64 = 500;
	const CACHE_CAPACITY: usize = 1000;

	// priority-lfu setup (~100 bytes per entry)
	let lfu_cache = Arc::new(Cache::new(CACHE_CAPACITY * 100));
	for i in 0..NUM_ITEMS {
		lfu_cache.insert(
			BenchKey(i),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	// quick_cache setup
	let quick_cache = Arc::new(QuickCache::new(CACHE_CAPACITY));
	for i in 0..NUM_ITEMS {
		quick_cache.insert(i, vec![0u8; 64]);
	}

	group.bench_function("priority_lfu", |b| {
		b.iter(|| {
			for i in 0..100u64 {
				if i % 5 == 0 {
					// 20% writes
					lfu_cache.insert(
						BenchKey(black_box(i)),
						BenchValue {
							data: vec![0u8; 64],
						},
					);
				} else {
					// 80% reads
					black_box(lfu_cache.get(&BenchKey(black_box(i % NUM_ITEMS))));
				}
			}
		});
	});

	group.bench_function("quick_cache", |b| {
		b.iter(|| {
			for i in 0..100u64 {
				if i % 5 == 0 {
					// 20% writes
					quick_cache.insert(black_box(i), vec![0u8; 64]);
				} else {
					// 80% reads
					black_box(quick_cache.get(&black_box(i % NUM_ITEMS)));
				}
			}
		});
	});

	group.finish();
}

fn bench_comparison_concurrent_reads(c: &mut Criterion) {
	use std::thread;

	let mut group = c.benchmark_group("comparison/concurrent_reads_4_threads");

	// Use same item capacity for fair comparison
	const NUM_ITEMS: u64 = 1000;
	const CACHE_CAPACITY: usize = 2000;

	// priority-lfu setup (~100 bytes per entry)
	let lfu_cache = Arc::new(Cache::new(CACHE_CAPACITY * 100));
	for i in 0..NUM_ITEMS {
		lfu_cache.insert(
			BenchKey(i),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	// quick_cache setup
	let quick_cache = Arc::new(QuickCache::new(CACHE_CAPACITY));
	for i in 0..NUM_ITEMS {
		quick_cache.insert(i, vec![0u8; 64]);
	}

	group.bench_function("priority_lfu", |b| {
		b.iter(|| {
			let mut handles = vec![];

			for _ in 0..4 {
				let cache = lfu_cache.clone();
				handles.push(thread::spawn(move || {
					for i in 0..250u64 {
						black_box(cache.get(&BenchKey(black_box(i))));
					}
				}));
			}

			for handle in handles {
				handle.join().expect("thread should not panic");
			}
		});
	});

	group.bench_function("quick_cache", |b| {
		b.iter(|| {
			let mut handles = vec![];

			for _ in 0..4 {
				let cache = quick_cache.clone();
				handles.push(thread::spawn(move || {
					for i in 0..250u64 {
						black_box(cache.get(&black_box(i)));
					}
				}));
			}

			for handle in handles {
				handle.join().expect("thread should not panic");
			}
		});
	});

	group.finish();
}

fn bench_comparison_eviction_pressure(c: &mut Criterion) {
	let mut group = c.benchmark_group("comparison/eviction_pressure");

	// Use same effective item capacity for fair comparison
	// Both caches should hold ~100 items before eviction starts
	const CACHE_CAPACITY: usize = 100;

	group.bench_function("priority_lfu", |b| {
		b.iter(|| {
			// ~150 bytes per entry (100 byte value + key + Arc overhead)
			let cache = Cache::new(CACHE_CAPACITY * 150);

			// Insert many items, forcing eviction
			for i in 0..1000u64 {
				cache.insert(
					BenchKey(black_box(i)),
					BenchValue {
						data: vec![0u8; 100],
					},
				);
			}
		});
	});

	group.bench_function("quick_cache", |b| {
		b.iter(|| {
			let cache = QuickCache::new(CACHE_CAPACITY);

			// Insert many items, forcing eviction
			for i in 0..1000u64 {
				cache.insert(black_box(i), vec![0u8; 100]);
			}
		});
	});

	group.finish();
}

fn bench_comparison_zipf_distribution(c: &mut Criterion) {
	let mut group = c.benchmark_group("comparison/zipf_distribution");

	// Use same item capacity for fair comparison
	const CACHE_CAPACITY: usize = 200;

	// Simulate Zipf distribution: some keys are accessed much more frequently
	let zipf_keys: Vec<u64> = (0..100)
		.flat_map(|i| {
			let freq = 100 / (i + 1); // First key appears 100 times, second 50 times, etc.
			vec![i; freq as usize]
		})
		.collect();

	// priority-lfu setup (~100 bytes per entry)
	let lfu_cache = Arc::new(Cache::new(CACHE_CAPACITY * 100));

	// quick_cache setup
	let quick_cache = Arc::new(QuickCache::new(CACHE_CAPACITY));

	group.bench_function("priority_lfu", |b| {
		b.iter(|| {
			for &key_id in &zipf_keys {
				let key = BenchKey(black_box(key_id));
				match lfu_cache.get(&key) {
					Some(val) => {
						black_box(val);
					}
					None => {
						lfu_cache.insert(
							key,
							BenchValue {
								data: vec![0u8; 64],
							},
						);
					}
				}
			}
		});
	});

	group.bench_function("quick_cache", |b| {
		b.iter(|| {
			for &key_id in &zipf_keys {
				let key = black_box(key_id);
				match quick_cache.get(&key) {
					Some(val) => {
						black_box(val);
					}
					None => {
						quick_cache.insert(key, vec![0u8; 64]);
					}
				}
			}
		});
	});

	group.finish();
}

// ============================================================================
// Borrowed Key Lookup Benchmarks: Demonstrates zero-allocation lookups
// ============================================================================

// String-based key for realistic benchmarking
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct StringKey(String, String);

impl CacheKey for StringKey {
	type Value = BenchValue;

	fn policy(&self) -> CachePolicy {
		CachePolicy::Standard
	}
}

// Borrowed lookup type
struct StringKeyRef<'a>(&'a str, &'a str);

impl std::hash::Hash for StringKeyRef<'_> {
	fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
		self.0.hash(state);
		self.1.hash(state);
	}
}

impl CacheKeyLookup<StringKey> for StringKeyRef<'_> {
	fn eq_key(&self, key: &StringKey) -> bool {
		self.0 == key.0 && self.1 == key.1
	}
}

fn bench_owned_vs_borrowed_get(c: &mut Criterion) {
	let mut group = c.benchmark_group("borrowed_key/get");

	let cache = Arc::new(Cache::new(1024 * 1024));

	// Pre-populate cache with string keys
	for i in 0..1000 {
		cache.insert(
			StringKey(format!("namespace_{}", i), format!("database_{}", i)),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	// Benchmark: get with owned key (requires allocation)
	group.bench_function("owned_key", |b| {
		b.iter(|| {
			for i in 0..1000 {
				let ns = format!("namespace_{}", black_box(i));
				let db = format!("database_{}", black_box(i));
				let key = StringKey(ns, db);
				black_box(cache.get(&key));
			}
		});
	});

	// Benchmark: get_by with borrowed key (zero allocation)
	group.bench_function("borrowed_key", |b| {
		b.iter(|| {
			for i in 0..1000 {
				let ns = format!("namespace_{}", black_box(i));
				let db = format!("database_{}", black_box(i));
				let key_ref = StringKeyRef(&ns, &db);
				black_box(cache.get_by::<StringKey, _>(&key_ref));
			}
		});
	});

	group.finish();
}

fn bench_owned_vs_borrowed_get_clone(c: &mut Criterion) {
	let mut group = c.benchmark_group("borrowed_key/get_clone");

	let cache = Arc::new(Cache::new(1024 * 1024));

	// Pre-populate cache with string keys
	for i in 0..1000 {
		cache.insert(
			StringKey(format!("namespace_{}", i), format!("database_{}", i)),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	// Benchmark: get_clone with owned key (requires allocation)
	group.bench_function("owned_key", |b| {
		b.iter(|| {
			for i in 0..1000 {
				let ns = format!("namespace_{}", black_box(i));
				let db = format!("database_{}", black_box(i));
				let key = StringKey(ns, db);
				black_box(cache.get_clone(&key));
			}
		});
	});

	// Benchmark: get_clone_by with borrowed key (zero allocation)
	group.bench_function("borrowed_key", |b| {
		b.iter(|| {
			for i in 0..1000 {
				let ns = format!("namespace_{}", black_box(i));
				let db = format!("database_{}", black_box(i));
				let key_ref = StringKeyRef(&ns, &db);
				black_box(cache.get_clone_by::<StringKey, _>(&key_ref));
			}
		});
	});

	group.finish();
}

fn bench_owned_vs_borrowed_contains(c: &mut Criterion) {
	let mut group = c.benchmark_group("borrowed_key/contains");

	let cache = Arc::new(Cache::new(1024 * 1024));

	// Pre-populate cache with string keys
	for i in 0..1000 {
		cache.insert(
			StringKey(format!("namespace_{}", i), format!("database_{}", i)),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	// Benchmark: contains with owned key (requires allocation)
	group.bench_function("owned_key", |b| {
		b.iter(|| {
			for i in 0..1000 {
				let ns = format!("namespace_{}", black_box(i));
				let db = format!("database_{}", black_box(i));
				let key = StringKey(ns, db);
				black_box(cache.contains(&key));
			}
		});
	});

	// Benchmark: contains_by with borrowed key (zero allocation)
	group.bench_function("borrowed_key", |b| {
		b.iter(|| {
			for i in 0..1000 {
				let ns = format!("namespace_{}", black_box(i));
				let db = format!("database_{}", black_box(i));
				let key_ref = StringKeyRef(&ns, &db);
				black_box(cache.contains_by::<StringKey, _>(&key_ref));
			}
		});
	});

	group.finish();
}

fn bench_borrowed_key_realistic_workload(c: &mut Criterion) {
	let mut group = c.benchmark_group("borrowed_key/realistic_workload");

	let cache = Arc::new(Cache::new(1024 * 1024));

	// Pre-populate with realistic database cache keys
	for i in 0..500 {
		cache.insert(
			StringKey(format!("ns_{}", i % 10), format!("db_{}", i)),
			BenchValue {
				data: vec![0u8; 128],
			},
		);
	}

	// Benchmark: Owned key approach (allocates on every lookup)
	group.bench_function("owned_key", |b| {
		b.iter(|| {
			// Simulate 100 cache operations
			for i in 0..100 {
				let ns = format!("ns_{}", black_box(i % 10));
				let db = format!("db_{}", black_box(i % 500));

				if i % 5 == 0 {
					// 20% writes
					cache.insert(
						StringKey(ns.clone(), db.clone()),
						BenchValue {
							data: vec![0u8; 128],
						},
					);
				} else {
					// 80% reads - requires cloning strings even just to check
					let key = StringKey(ns, db);
					if let Some(val) = cache.get_clone(&key) {
						black_box(val);
					}
				}
			}
		});
	});

	// Benchmark: Borrowed key approach (zero allocation for reads)
	group.bench_function("borrowed_key", |b| {
		b.iter(|| {
			// Simulate 100 cache operations
			for i in 0..100 {
				let ns = format!("ns_{}", black_box(i % 10));
				let db = format!("db_{}", black_box(i % 500));

				if i % 5 == 0 {
					// 20% writes
					cache.insert(
						StringKey(ns.clone(), db.clone()),
						BenchValue {
							data: vec![0u8; 128],
						},
					);
				} else {
					// 80% reads - zero allocation lookup
					let key_ref = StringKeyRef(&ns, &db);
					if let Some(val) = cache.get_clone_by::<StringKey, _>(&key_ref) {
						black_box(val);
					}
				}
			}
		});
	});

	group.finish();
}

fn bench_borrowed_key_different_sizes(c: &mut Criterion) {
	let mut group = c.benchmark_group("borrowed_key/key_sizes");

	for key_size in [10, 50, 100, 200] {
		let cache = Arc::new(Cache::new(1024 * 1024));

		// Create keys of different sizes
		let large_prefix = "x".repeat(key_size);

		// Pre-populate
		for i in 0..100 {
			cache.insert(
				StringKey(format!("{}{}", large_prefix, i), format!("db_{}", i)),
				BenchValue {
					data: vec![0u8; 64],
				},
			);
		}

		group.bench_with_input(BenchmarkId::new("owned_key", key_size), &key_size, |b, &size| {
			let prefix = "x".repeat(size);
			b.iter(|| {
				for i in 0..100 {
					let key = StringKey(
						format!("{}{}", prefix, black_box(i)),
						format!("db_{}", black_box(i)),
					);
					black_box(cache.get(&key));
				}
			});
		});

		group.bench_with_input(
			BenchmarkId::new("borrowed_key", key_size),
			&key_size,
			|b, &size| {
				let prefix = "x".repeat(size);
				b.iter(|| {
					for i in 0..100 {
						let ns = format!("{}{}", prefix, black_box(i));
						let db = format!("db_{}", black_box(i));
						let key_ref = StringKeyRef(&ns, &db);
						black_box(cache.get_by::<StringKey, _>(&key_ref));
					}
				});
			},
		);
	}

	group.finish();
}

criterion_group!(
	benches,
	bench_insert,
	bench_get_hit,
	bench_get_vs_get_clone,
	bench_mixed_workload,
	bench_concurrent_reads,
	bench_eviction_pressure,
	bench_hit_rate_zipf,
	// Comparison benchmarks
	bench_comparison_insert,
	bench_comparison_get_hit,
	bench_comparison_mixed_workload,
	bench_comparison_concurrent_reads,
	bench_comparison_eviction_pressure,
	bench_comparison_zipf_distribution,
	// Borrowed key benchmarks
	bench_owned_vs_borrowed_get,
	bench_owned_vs_borrowed_get_clone,
	bench_owned_vs_borrowed_contains,
	bench_borrowed_key_realistic_workload,
	bench_borrowed_key_different_sizes
);

criterion_main!(benches);

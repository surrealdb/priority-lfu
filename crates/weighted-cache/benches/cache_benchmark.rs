use std::hint::black_box;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use quick_cache::sync::Cache as QuickCache;
use weighted_cache::{Cache, CacheKey, DeepSizeOf};

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct BenchKey(u64);

impl CacheKey for BenchKey {
	type Value = BenchValue;

	fn weight(&self) -> u64 {
		50
	}
}

#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
struct BenchValue {
	data: Vec<u8>,
}

fn bench_insert(c: &mut Criterion) {
	let mut group = c.benchmark_group("insert");

	for size in [100, 1000, 10000] {
		group.throughput(Throughput::Elements(size as u64));
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
				let _ = cache.get_arc(&key);
			}
		});
	});
}

fn bench_get_arc_vs_get_clone(c: &mut Criterion) {
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

	group.bench_function("get_arc", |b| {
		b.iter(|| {
			for i in 0..100 {
				let _ = cache.get_arc(&BenchKey(black_box(i)));
			}
		});
	});

	group.bench_function("get_clone", |b| {
		b.iter(|| {
			for i in 0..100 {
				let _ = cache.get_clone(&BenchKey(black_box(i)));
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
					let _ = cache.get_arc(&BenchKey(black_box(i % 500)));
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
						let _ = cache.get_arc(&BenchKey(i));
					}
				}));
			}

			for handle in handles {
				handle.join().unwrap();
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
				if cache.contains(&BenchKey(key_id)) {
					let _ = cache.get_arc(&BenchKey(key_id));
				} else {
					cache.insert(
						BenchKey(key_id),
						BenchValue {
							data: vec![0u8; 64],
						},
					);
				}
			}
		});
	});
}

// ============================================================================
// Comparison Benchmarks: weighted-cache vs quick_cache
// ============================================================================

fn bench_comparison_insert(c: &mut Criterion) {
	let mut group = c.benchmark_group("comparison/insert");

	for size in [100, 1000, 10000] {
		group.throughput(Throughput::Elements(size as u64));
		
		group.bench_with_input(
			BenchmarkId::new("weighted_cache", size),
			&size,
			|b, &size| {
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
			},
		);

		group.bench_with_input(
			BenchmarkId::new("quick_cache", size),
			&size,
			|b, &size| {
				b.iter(|| {
					let cache = QuickCache::new(10000);
					for i in 0..size {
						let key = i;
						let value = vec![0u8; 64];
						cache.insert(black_box(key), black_box(value));
					}
				});
			},
		);
	}

	group.finish();
}

fn bench_comparison_get_hit(c: &mut Criterion) {
	let mut group = c.benchmark_group("comparison/get_hit");

	// weighted-cache setup
	let weighted_cache = Arc::new(Cache::new(1024 * 1024));
	for i in 0..1000 {
		weighted_cache.insert(
			BenchKey(i),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	// quick_cache setup
	let quick_cache = Arc::new(QuickCache::new(10000));
	for i in 0..1000u64 {
		quick_cache.insert(i, vec![0u8; 64]);
	}

	group.bench_function("weighted_cache", |b| {
		b.iter(|| {
			for i in 0..1000 {
				let key = BenchKey(black_box(i));
				let _ = weighted_cache.get(&key);
			}
		});
	});

	group.bench_function("quick_cache", |b| {
		b.iter(|| {
			for i in 0..1000u64 {
				let _ = quick_cache.get(&black_box(i));
			}
		});
	});

	group.finish();
}

fn bench_comparison_mixed_workload(c: &mut Criterion) {
	let mut group = c.benchmark_group("comparison/mixed_80_20");

	// weighted-cache setup
	let weighted_cache = Arc::new(Cache::new(1024 * 1024));
	for i in 0..500 {
		weighted_cache.insert(
			BenchKey(i),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	// quick_cache setup
	let quick_cache = Arc::new(QuickCache::new(10000));
	for i in 0..500u64 {
		quick_cache.insert(i, vec![0u8; 64]);
	}

	group.bench_function("weighted_cache", |b| {
		b.iter(|| {
			for i in 0..100 {
				if i % 5 == 0 {
					// 20% writes
					weighted_cache.insert(
						BenchKey(black_box(i)),
						BenchValue {
							data: vec![0u8; 64],
						},
					);
				} else {
					// 80% reads
					let _ = weighted_cache.get(&BenchKey(black_box(i % 500)));
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
					let _ = quick_cache.get(&black_box(i % 500));
				}
			}
		});
	});

	group.finish();
}

fn bench_comparison_concurrent_reads(c: &mut Criterion) {
	use std::thread;

	let mut group = c.benchmark_group("comparison/concurrent_reads_4_threads");

	// weighted-cache setup
	let weighted_cache = Arc::new(Cache::new(1024 * 1024));
	for i in 0..1000 {
		weighted_cache.insert(
			BenchKey(i),
			BenchValue {
				data: vec![0u8; 64],
			},
		);
	}

	// quick_cache setup
	let quick_cache = Arc::new(QuickCache::new(10000));
	for i in 0..1000u64 {
		quick_cache.insert(i, vec![0u8; 64]);
	}

	group.bench_function("weighted_cache", |b| {
		b.iter(|| {
			let mut handles = vec![];

			for _ in 0..4 {
				let cache = weighted_cache.clone();
				handles.push(thread::spawn(move || {
					for i in 0..250 {
						let _ = cache.get(&BenchKey(i));
					}
				}));
			}

			for handle in handles {
				handle.join().unwrap();
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
						let _ = cache.get(&i);
					}
				}));
			}

			for handle in handles {
				handle.join().unwrap();
			}
		});
	});

	group.finish();
}

fn bench_comparison_eviction_pressure(c: &mut Criterion) {
	let mut group = c.benchmark_group("comparison/eviction_pressure");

	group.bench_function("weighted_cache", |b| {
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

	group.bench_function("quick_cache", |b| {
		b.iter(|| {
			let cache = QuickCache::new(100); // Small cache to trigger eviction

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

	// Simulate Zipf distribution: some keys are accessed much more frequently
	let zipf_keys: Vec<u64> = (0..100)
		.flat_map(|i| {
			let freq = 100 / (i + 1); // First key appears 100 times, second 50 times, etc.
			vec![i; freq as usize]
		})
		.collect();

	// weighted-cache setup
	let weighted_cache = Arc::new(Cache::new(1024 * 1024));

	// quick_cache setup
	let quick_cache = Arc::new(QuickCache::new(10000));

	group.bench_function("weighted_cache", |b| {
		b.iter(|| {
			for &key_id in &zipf_keys {
				if weighted_cache.contains(&BenchKey(key_id)) {
					let _ = weighted_cache.get(&BenchKey(key_id));
				} else {
					weighted_cache.insert(
						BenchKey(key_id),
						BenchValue {
							data: vec![0u8; 64],
						},
					);
				}
			}
		});
	});

	group.bench_function("quick_cache", |b| {
		b.iter(|| {
			for &key_id in &zipf_keys {
				if quick_cache.get(&key_id).is_some() {
					let _ = quick_cache.get(&key_id);
				} else {
					quick_cache.insert(key_id, vec![0u8; 64]);
				}
			}
		});
	});

	group.finish();
}

criterion_group!(
	benches,
	bench_insert,
	bench_get_hit,
	bench_get_arc_vs_get_clone,
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
	bench_comparison_zipf_distribution
);

criterion_main!(benches);

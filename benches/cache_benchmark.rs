use std::hint::black_box;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use priority_lfu::{Cache, CacheKey, DeepSizeOf};

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

criterion_group!(
	benches,
	bench_insert,
	bench_get_hit,
	bench_get_arc_vs_get_clone,
	bench_mixed_workload,
	bench_concurrent_reads,
	bench_eviction_pressure,
	bench_hit_rate_zipf
);

criterion_main!(benches);

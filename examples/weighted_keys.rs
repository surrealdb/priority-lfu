use weighted_cache::{Cache, CacheKey, CacheValue, DeepSizeOf};

/// Example demonstrating how different keys can have different weights
/// for the same value type.

#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
struct UserProfile {
	name: String,
	email: String,
}

impl CacheValue for UserProfile {}

// Premium users get high cache priority
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct PremiumUserId(u64);

impl CacheKey for PremiumUserId {
	type Value = UserProfile;

	fn weight(&self) -> u64 {
		1000 // Very high weight - resistant to eviction
	}
}

// Free users get low cache priority
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct FreeUserId(u64);

impl CacheKey for FreeUserId {
	type Value = UserProfile;

	fn weight(&self) -> u64 {
		10 // Low weight - more likely to be evicted
	}
}

fn main() {
	// Create a small cache to demonstrate eviction
	let cache = Cache::new(500);

	// Insert premium user
	cache.insert(
		PremiumUserId(1),
		UserProfile {
			name: "Alice Premium".to_string(),
			email: "alice@premium.com".to_string(),
		},
	);

	// Insert free user
	cache.insert(
		FreeUserId(2),
		UserProfile {
			name: "Bob Free".to_string(),
			email: "bob@free.com".to_string(),
		},
	);

	// Fill cache with more entries to trigger eviction
	for i in 3..20 {
		cache.insert(
			FreeUserId(i),
			UserProfile {
				name: format!("User {}", i),
				email: format!("user{}@free.com", i),
			},
		);
	}

	// Premium user should still be in cache (high weight)
	if cache.contains(&PremiumUserId(1)) {
		println!("✓ Premium user (high weight) survived eviction");
	} else {
		println!("✗ Premium user was evicted (unexpected)");
	}

	// Original free user is likely evicted (low weight)
	if cache.contains(&FreeUserId(2)) {
		println!("✓ Free user still in cache");
	} else {
		println!("✗ Free user was evicted (expected due to low weight)");
	}

	println!("\nCache stats:");
	println!("  Entries: {}", cache.len());
	println!("  Size: {} bytes", cache.size());
}

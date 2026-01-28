use weighted_cache::{Cache, CacheKey, CachePolicy, DeepSizeOf};

/// Example demonstrating how different keys can have different eviction policies
/// for the same value type.

#[derive(Clone, Debug, PartialEq, DeepSizeOf)]
struct UserProfile {
	name: String,
	email: String,
}

// Premium users get high cache priority
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct PremiumUserId(u64);

impl CacheKey for PremiumUserId {
	type Value = UserProfile;

	fn policy(&self) -> CachePolicy {
		CachePolicy::Critical // High priority - resistant to eviction
	}
}

// Free users get low cache priority
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct FreeUserId(u64);

impl CacheKey for FreeUserId {
	type Value = UserProfile;

	fn policy(&self) -> CachePolicy {
		CachePolicy::Volatile // Low priority - more likely to be evicted
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

	// Premium user should still be in cache (high priority)
	if cache.contains(&PremiumUserId(1)) {
		println!("✓ Premium user (high priority) survived eviction");
	} else {
		println!("✗ Premium user was evicted (unexpected)");
	}

	// Original free user is likely evicted (low priority)
	if cache.contains(&FreeUserId(2)) {
		println!("✓ Free user still in cache");
	} else {
		println!("✗ Free user was evicted (expected due to low priority)");
	}

	println!("\nCache stats:");
	println!("  Entries: {}", cache.len());
	println!("  Size: {} bytes", cache.size());
}

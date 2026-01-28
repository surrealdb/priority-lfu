use std::marker::PhantomData;
use std::ops::Deref;

use parking_lot::RwLockReadGuard;

/// RAII guard for borrowed values. Holds a read lock on the shard.
///
/// **Intentionally `!Send`** to prevent holding across `.await` points,
/// which could cause deadlocks or excessive lock contention.
///
/// For async contexts, use `Cache::get_arc()` instead.
///
/// # Example
///
/// ```ignore
/// // ✅ Good: Guard is dropped before await
/// let data = {
///     let guard = cache.get(&key)?;
///     guard.some_field.clone()
/// };
/// some_async_fn(data).await;
///
/// // ❌ Bad: Would hold lock across await (won't compile in Send context)
/// let guard = cache.get(&key)?;
/// some_async_fn().await;
/// println!("{:?}", *guard);
/// ```
pub struct Guard<'a, V> {
	/// The read lock guard (makes this type !Send automatically)
	#[allow(unused)]
	lock: RwLockReadGuard<'a, crate::shard::Shard>,
	/// Pointer to the value within the locked shard
	value: *const V,
	/// Phantom data to tie the lifetime and type
	marker: PhantomData<&'a V>,
}

impl<'a, V> Guard<'a, V> {
	/// Create a new guard.
	///
	/// # Safety
	///
	/// The caller must ensure that `value` points to a valid `V` that lives
	/// at least as long as the lock guard.
	pub(crate) unsafe fn new(
		lock: RwLockReadGuard<'a, crate::shard::Shard>,
		value: *const V,
	) -> Self {
		Self {
			lock,
			value,
			marker: PhantomData,
		}
	}
}

// Guard is Sync (can be shared by reference across threads) but NOT Send
// This is automatic because RwLockReadGuard from parking_lot is !Send
unsafe impl<V: Sync> Sync for Guard<'_, V> {}

// Explicitly NOT implementing Send - this is intentional!
// The compiler automatically makes this !Send because RwLockReadGuard is !Send

impl<V> Deref for Guard<'_, V> {
	type Target = V;

	fn deref(&self) -> &V {
		// SAFETY: The guard holds a read lock on the shard, so the value is valid
		// and won't be modified or dropped while this guard exists.
		unsafe { &*self.value }
	}
}

// Implement common traits for convenience
impl<V: std::fmt::Debug> std::fmt::Debug for Guard<'_, V> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		(**self).fmt(f)
	}
}

impl<V: std::fmt::Display> std::fmt::Display for Guard<'_, V> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		(**self).fmt(f)
	}
}

impl<V: PartialEq> PartialEq for Guard<'_, V> {
	fn eq(&self, other: &Self) -> bool {
		**self == **other
	}
}

impl<V: Eq> Eq for Guard<'_, V> {}

impl<V: PartialOrd> PartialOrd for Guard<'_, V> {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		(**self).partial_cmp(&**other)
	}
}

impl<V: Ord> Ord for Guard<'_, V> {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		(**self).cmp(&**other)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// Test that Guard is Sync
	#[test]
	fn test_guard_is_sync() {
		fn assert_sync<T: Sync>() {}
		assert_sync::<Guard<i32>>();
	}
}

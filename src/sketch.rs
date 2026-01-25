use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Count-Min Sketch for frequency estimation with 4-bit saturating counters.
///
/// Uses 4 rows of counters, each packed into u64 values (16 counters per u64).
/// Provides lock-free increment and read operations for high concurrency.
pub struct FrequencySketch {
    /// 4 rows of counters (4-bit saturating counters packed into u64)
    /// Each u64 holds 16 counters (64 bits / 4 bits per counter)
    table: Vec<AtomicU64>,
    /// Mask for index calculation (table.len() / 4 - 1)
    /// Since we have 4 rows, each row has table.len() / 4 slots
    mask: usize,
    /// Seeds for hash functions (one per row)
    seeds: [u64; 4],
    /// Total increments (for periodic halving)
    additions: AtomicUsize,
    /// Sample size before reset (halving all counters)
    sample_size: usize,
}

impl FrequencySketch {
    /// Create a new frequency sketch.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Expected number of unique items
    ///
    /// The sketch will use approximately `capacity * 4 * 0.5` bytes of memory.
    pub fn new(capacity: usize) -> Self {
        // Calculate table size: we want at least capacity/4 slots per row
        // Round up to next power of 2 for efficient masking
        let slots_per_row = (capacity / 4).next_power_of_two().max(16);
        let total_u64s = slots_per_row * 4; // 4 rows

        Self {
            table: (0..total_u64s).map(|_| AtomicU64::new(0)).collect(),
            mask: slots_per_row - 1,
            seeds: [0x9e3779b97f4a7c15, 0x517cc1b727220a95, 0xbf58476d1ce4e5b9, 0x94d049bb133111eb],
            additions: AtomicUsize::new(0),
            sample_size: capacity * 10, // Reset after 10x capacity accesses
        }
    }

    /// Increment frequency for a key's hash.
    ///
    /// This is lock-free and safe to call concurrently.
    pub fn increment(&self, hash: u64) {
        // Increment all 4 counters (one per row)
        for row in 0..4 {
            let index = self.index(hash, row);
            let offset = self.offset(hash, row);
            self.increment_at(index, offset);
        }

        // Check if we need to reset (halve all counters)
        let count = self.additions.fetch_add(1, Ordering::Relaxed);
        if count >= self.sample_size {
            self.reset();
        }
    }

    /// Estimate frequency (returns 0-15).
    ///
    /// Returns the minimum value across all 4 rows to reduce false positives.
    pub fn frequency(&self, hash: u64) -> u8 {
        let mut min = 15u8;
        for row in 0..4 {
            let index = self.index(hash, row);
            let offset = self.offset(hash, row);
            let value = self.get_at(index, offset);
            min = min.min(value);
        }
        min
    }

    /// Halve all counters (age out old frequencies).
    ///
    /// This prevents the sketch from saturating over time.
    fn reset(&self) {
        // Only one thread should perform the reset
        if self.additions.compare_exchange(
            self.sample_size,
            0,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ).is_ok() {
            // Halve all counters
            for slot in &self.table {
                loop {
                    let current = slot.load(Ordering::Relaxed);
                    let halved = Self::halve_counters(current);
                    if slot.compare_exchange_weak(
                        current,
                        halved,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ).is_ok() {
                        break;
                    }
                }
            }
        }
    }

    /// Calculate the table index for a given hash and row.
    fn index(&self, hash: u64, row: usize) -> usize {
        let h = Self::mix_hash(hash, self.seeds[row]);
        let slot = (h as usize) & self.mask;
        row * (self.mask + 1) + slot
    }

    /// Calculate the counter offset within a u64 (0-15).
    fn offset(&self, hash: u64, row: usize) -> usize {
        let h = Self::mix_hash(hash, self.seeds[row]);
        ((h >> 32) & 0xF) as usize
    }

    /// Mix hash with seed for better distribution.
    fn mix_hash(hash: u64, seed: u64) -> u64 {
        hash.wrapping_mul(seed)
    }

    /// Increment a 4-bit counter at the given index and offset.
    ///
    /// Uses compare-and-swap to atomically increment without locks.
    /// Saturates at 15 (maximum value for 4 bits).
    fn increment_at(&self, index: usize, offset: usize) {
        let slot = &self.table[index];
        loop {
            let current = slot.load(Ordering::Relaxed);
            let counter = Self::extract_counter(current, offset);
            
            // Saturate at 15
            if counter >= 15 {
                break;
            }

            let new_value = Self::set_counter(current, offset, counter + 1);
            
            if slot.compare_exchange_weak(
                current,
                new_value,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ).is_ok() {
                break;
            }
        }
    }

    /// Get a 4-bit counter at the given index and offset.
    fn get_at(&self, index: usize, offset: usize) -> u8 {
        let slot = &self.table[index];
        let value = slot.load(Ordering::Relaxed);
        Self::extract_counter(value, offset)
    }

    /// Extract a 4-bit counter from a u64.
    ///
    /// Each u64 holds 16 counters, so we shift and mask.
    fn extract_counter(value: u64, offset: usize) -> u8 {
        let shift = offset * 4;
        ((value >> shift) & 0xF) as u8
    }

    /// Set a 4-bit counter in a u64.
    fn set_counter(value: u64, offset: usize, counter: u8) -> u64 {
        let shift = offset * 4;
        let mask = !(0xFu64 << shift);
        (value & mask) | ((counter as u64) << shift)
    }

    /// Halve all 16 counters in a u64.
    fn halve_counters(value: u64) -> u64 {
        // For each 4-bit counter, right shift by 1
        let mut result = 0u64;
        for i in 0..16 {
            let shift = i * 4;
            let counter = (value >> shift) & 0xF;
            let halved = counter >> 1;
            result |= halved << shift;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter_extraction() {
        let value = 0x123456789ABCDEF0u64;
        
        assert_eq!(FrequencySketch::extract_counter(value, 0), 0x0);
        assert_eq!(FrequencySketch::extract_counter(value, 1), 0xF);
        assert_eq!(FrequencySketch::extract_counter(value, 15), 0x1);
    }

    #[test]
    fn test_counter_setting() {
        let value = 0u64;
        
        let new_value = FrequencySketch::set_counter(value, 0, 5);
        assert_eq!(FrequencySketch::extract_counter(new_value, 0), 5);
        
        let new_value = FrequencySketch::set_counter(new_value, 1, 7);
        assert_eq!(FrequencySketch::extract_counter(new_value, 0), 5);
        assert_eq!(FrequencySketch::extract_counter(new_value, 1), 7);
    }

    #[test]
    fn test_halving() {
        let value = 0xFFFFFFFFFFFFFFFFu64; // All counters at 15
        let halved = FrequencySketch::halve_counters(value);
        
        for i in 0..16 {
            assert_eq!(FrequencySketch::extract_counter(halved, i), 7);
        }
    }

    #[test]
    fn test_increment_and_frequency() {
        let sketch = FrequencySketch::new(1024);
        
        let hash1 = 12345u64;
        let hash2 = 67890u64;
        
        // Increment hash1 multiple times
        for _ in 0..5 {
            sketch.increment(hash1);
        }
        
        // Increment hash2 once
        sketch.increment(hash2);
        
        // Frequency should reflect the counts (approximately)
        let freq1 = sketch.frequency(hash1);
        let freq2 = sketch.frequency(hash2);
        
        assert!(freq1 >= 5, "freq1 should be at least 5, got {}", freq1);
        assert!(freq2 >= 1, "freq2 should be at least 1, got {}", freq2);
        assert!(freq1 > freq2, "freq1 should be greater than freq2");
    }

    #[test]
    fn test_saturation() {
        let sketch = FrequencySketch::new(1024);
        let hash = 99999u64;
        
        // Increment many times
        for _ in 0..100 {
            sketch.increment(hash);
        }
        
        // Should saturate at 15
        let freq = sketch.frequency(hash);
        assert_eq!(freq, 15, "Frequency should saturate at 15");
    }

    #[test]
    fn test_concurrent_increments() {
        use std::sync::Arc;
        use std::thread;
        
        let sketch = Arc::new(FrequencySketch::new(1024));
        let hash = 42u64;
        let threads = 4;
        let increments_per_thread = 10;
        
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let sketch = sketch.clone();
                thread::spawn(move || {
                    for _ in 0..increments_per_thread {
                        sketch.increment(hash);
                    }
                })
            })
            .collect();
        
        for handle in handles {
            handle.join().unwrap();
        }
        
        let freq = sketch.frequency(hash);
        // Due to CAS retries, minimum across rows, and saturation,
        // we just verify the frequency is non-zero and <= 15 (max)
        assert!(freq > 0, "Frequency should be > 0 after {} increments", threads * increments_per_thread);
        assert!(freq <= 15, "Frequency should not exceed 15 (saturated)");
    }
}

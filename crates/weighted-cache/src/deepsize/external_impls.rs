#[cfg(feature = "slotmap")]
mod slotmap_impl {
	use core::mem::size_of;

	use crate::{Context, DeepSizeOf, known_deep_size};

	known_deep_size!(0; slotmap::KeyData, slotmap::DefaultKey);

	impl<K, V> DeepSizeOf for slotmap::SlotMap<K, V>
	where
		K: DeepSizeOf + slotmap::Key,
		V: DeepSizeOf + slotmap::Slottable,
	{
		fn deep_size_of_children(&self, context: &mut Context) -> usize {
			self.iter().fold(0, |sum, (key, val)| {
				sum + key.deep_size_of_children(context) + val.deep_size_of_children(context)
			}) + self.capacity() * size_of::<(u32, V)>()
		}
	}
}

#[cfg(feature = "slab")]
mod slab_impl {
	use core::mem::size_of;

	use crate::{Context, DeepSizeOf};

	// Mirror's `slab`'s internal `Entry` struct
	enum MockEntry<T> {
		_Vacant(usize),
		_Occupied(T),
	}

	impl<T> DeepSizeOf for slab::Slab<T>
	where
		T: DeepSizeOf,
	{
		fn deep_size_of_children(&self, context: &mut Context) -> usize {
			let capacity_size = self.capacity() * size_of::<MockEntry<T>>();
			let owned_size =
				self.iter().fold(0, |sum, (_, val)| sum + val.deep_size_of_children(context));
			capacity_size + owned_size
		}
	}
}

#[cfg(feature = "arrayvec")]
mod arrayvec_impl {
	use crate::{Context, DeepSizeOf, known_deep_size};

	impl<A> DeepSizeOf for arrayvec::ArrayVec<A>
	where
		A: arrayvec::Array,
		<A as arrayvec::Array>::Item: DeepSizeOf,
	{
		fn deep_size_of_children(&self, context: &mut Context) -> usize {
			self.iter().fold(0, |sum, elem| sum + elem.deep_size_of_children(context))
		}
	}

	known_deep_size!(0; { A: arrayvec::Array<Item=u8> + Copy } arrayvec::ArrayString<A>);
}

#[cfg(feature = "smallvec")]
mod smallvec_impl {
	use core::mem::size_of;

	use crate::{Context, DeepSizeOf};

	impl<A> DeepSizeOf for smallvec::SmallVec<A>
	where
		A: smallvec::Array,
		<A as smallvec::Array>::Item: DeepSizeOf,
	{
		fn deep_size_of_children(&self, context: &mut Context) -> usize {
			let child_size =
				self.iter().fold(0, |sum, elem| sum + elem.deep_size_of_children(context));
			if self.spilled() {
				child_size + self.capacity() * size_of::<<A as smallvec::Array>::Item>()
			} else {
				child_size
			}
		}
	}
}

mod hashbrown_impl {
	use core::mem::size_of;

	use crate::{Context, DeepSizeOf};

	// This is probably still incorrect, but it's better than before
	impl<K, V, S> DeepSizeOf for hashbrown::HashMap<K, V, S>
	where
		K: DeepSizeOf + Eq + std::hash::Hash,
		V: DeepSizeOf,
		S: std::hash::BuildHasher,
	{
		fn deep_size_of_children(&self, context: &mut Context) -> usize {
			self.iter().fold(0, |sum, (key, val)| {
				sum + key.deep_size_of_children(context) + val.deep_size_of_children(context)
			}) + self.capacity() * size_of::<(K, V)>()
			// Buckets would be the more correct value, but there isn't
			// an API for accessing that with hashbrown.
			// I believe that hashbrown's HashTable is represented as
			// an array of (K, V), with control bytes at the start/end
			// that mark used/uninitialized buckets (?)
		}
	}

	impl<K, S> DeepSizeOf for hashbrown::HashSet<K, S>
	where
		K: DeepSizeOf + Eq + std::hash::Hash,
		S: std::hash::BuildHasher,
	{
		fn deep_size_of_children(&self, context: &mut Context) -> usize {
			self.iter().fold(0, |sum, key| sum + key.deep_size_of_children(context))
				+ self.capacity() * size_of::<K>()
		}
	}
}

#[cfg(feature = "chrono")]
mod chrono_impl {
	use chrono::*;

	use crate::known_deep_size;

	known_deep_size!(0;
		NaiveDate, NaiveTime, NaiveDateTime, IsoWeek,
		Duration, Month, Weekday,
		FixedOffset, Local, Utc,
		{T: TimeZone} DateTime<T>
	);
}

#[cfg(feature = "uuid")]
mod uuid_impl {
	use uuid::Uuid;

	use crate::known_deep_size;

	known_deep_size!(0; Uuid);
}

#[cfg(feature = "geo")]
mod geo_impl {
	use crate::known_deep_size;

	known_deep_size!(0; geo::Point, geo::LineString, geo::Polygon, geo::MultiPoint, geo::MultiLineString, geo::MultiPolygon);
}

#[cfg(feature = "regex")]
mod regex_impl {
	use crate::known_deep_size;

	known_deep_size!(0; regex::Regex);
}

#[cfg(feature = "rust_decimal")]
mod rust_decimal_impl {
	use crate::known_deep_size;

	known_deep_size!(0; rust_decimal::Decimal);
}

#[cfg(feature = "bytes")]
mod bytes_impl {
	use crate::known_deep_size;

	known_deep_size!(0; bytes::Bytes);
}

//! Rust stand-in for the family of JS `TypedArray` subclasses geotiff.js
//! dispatches over at runtime (`instanceof Uint16Array` etc). JS represents
//! "an array of numbers of some specific numeric width" as one dynamic
//! runtime value whose concrete class is checked with reflection; Rust has
//! no equivalent dynamic-numeric-array value, so the faithful translation is
//! a closed enum over the concrete element types actually used in this
//! codebase, matched instead of reflected on. The classification behavior
//! (which kinds count as "float"/"int"/"uint") is preserved exactly - see
//! `is_typed_float_array` / `is_typed_int_array` / `is_typed_uint_array`
//! below, ported from utils.ts.

#[derive(Debug, Clone, PartialEq)]
pub enum TypedArray {
    Int8(Vec<i8>),
    Uint8(Vec<u8>),
    Uint8Clamped(Vec<u8>),
    Int16(Vec<i16>),
    Uint16(Vec<u16>),
    Int32(Vec<i32>),
    Uint32(Vec<u32>),
    /// No JS `TypedArray` equivalent in this codebase: BigTIFF's LONG8/
    /// SLONG8/IFD8 field types fall back to a plain `Array` in
    /// imagefiledirectory.js `getArrayForSamples` (JS has no
    /// `Uint64Array`/`BigInt64Array` usage here), which - like
    /// `DataView64`/`DataSlice`'s 64-bit reads - is a workaround for JS's
    /// f64-based `number` type, not a semantic requirement. Native `u64`/
    /// `i64` are the direct, more precise Rust equivalent.
    Uint64(Vec<u64>),
    Int64(Vec<i64>),
    Float32(Vec<f32>),
    Float64(Vec<f64>),
}

#[derive(Clone, Copy)]
enum TypedValue {
    Signed(i64),
    Unsigned(u64),
    Float(f64),
}

fn to_uint_width(value: f64, bits: u32) -> u64 {
    if !value.is_finite() || value == 0.0 {
        return 0;
    }
    value.trunc().rem_euclid(2f64.powi(bits as i32)) as u64
}

fn to_uint8_clamp(value: f64) -> u8 {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if value >= 255.0 {
        return 255;
    }
    let floor = value.floor();
    let difference = value - floor;
    if difference < 0.5 || (difference == 0.5 && (floor as u64).is_multiple_of(2)) {
        floor as u8
    } else {
        (floor + 1.0) as u8
    }
}

impl TypedArray {
    pub fn len(&self) -> usize {
        match self {
            TypedArray::Int8(v) => v.len(),
            TypedArray::Uint8(v) => v.len(),
            TypedArray::Uint8Clamped(v) => v.len(),
            TypedArray::Int16(v) => v.len(),
            TypedArray::Uint16(v) => v.len(),
            TypedArray::Int32(v) => v.len(),
            TypedArray::Uint32(v) => v.len(),
            TypedArray::Uint64(v) => v.len(),
            TypedArray::Int64(v) => v.len(),
            TypedArray::Float32(v) => v.len(),
            TypedArray::Float64(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// resample.js `copyNewSize`: `new (Object.getPrototypeOf(array).constructor)(len)`
    /// - a same-concrete-type, zero-filled array of the given length.
    pub fn new_zeroed(&self, len: usize) -> TypedArray {
        match self {
            TypedArray::Int8(_) => TypedArray::Int8(vec![0; len]),
            TypedArray::Uint8(_) => TypedArray::Uint8(vec![0; len]),
            TypedArray::Uint8Clamped(_) => TypedArray::Uint8Clamped(vec![0; len]),
            TypedArray::Int16(_) => TypedArray::Int16(vec![0; len]),
            TypedArray::Uint16(_) => TypedArray::Uint16(vec![0; len]),
            TypedArray::Int32(_) => TypedArray::Int32(vec![0; len]),
            TypedArray::Uint32(_) => TypedArray::Uint32(vec![0; len]),
            TypedArray::Uint64(_) => TypedArray::Uint64(vec![0; len]),
            TypedArray::Int64(_) => TypedArray::Int64(vec![0; len]),
            TypedArray::Float32(_) => TypedArray::Float32(vec![0.0; len]),
            TypedArray::Float64(_) => TypedArray::Float64(vec![0.0; len]),
        }
    }

    /// Fallible form used for sizes derived from untrusted TIFF metadata or
    /// caller-provided windows.
    pub fn try_new_zeroed(
        &self,
        len: usize,
    ) -> Result<TypedArray, std::collections::TryReserveError> {
        fn zeroed<T: Default + Clone>(
            len: usize,
        ) -> Result<Vec<T>, std::collections::TryReserveError> {
            let mut values = Vec::new();
            values.try_reserve_exact(len)?;
            values.resize(len, T::default());
            Ok(values)
        }

        Ok(match self {
            TypedArray::Int8(_) => TypedArray::Int8(zeroed(len)?),
            TypedArray::Uint8(_) => TypedArray::Uint8(zeroed(len)?),
            TypedArray::Uint8Clamped(_) => TypedArray::Uint8Clamped(zeroed(len)?),
            TypedArray::Int16(_) => TypedArray::Int16(zeroed(len)?),
            TypedArray::Uint16(_) => TypedArray::Uint16(zeroed(len)?),
            TypedArray::Int32(_) => TypedArray::Int32(zeroed(len)?),
            TypedArray::Uint32(_) => TypedArray::Uint32(zeroed(len)?),
            TypedArray::Uint64(_) => TypedArray::Uint64(zeroed(len)?),
            TypedArray::Int64(_) => TypedArray::Int64(zeroed(len)?),
            TypedArray::Float32(_) => TypedArray::Float32(zeroed(len)?),
            TypedArray::Float64(_) => TypedArray::Float64(zeroed(len)?),
        })
    }

    /// Reads element `i` widened to f64, for generic numeric code (e.g.
    /// rgb.ts) that only cares about the numeric value, not the storage
    /// width - matches how JS code reads any typed array element as a
    /// plain `number` regardless of its concrete class.
    pub fn get_f64(&self, i: usize) -> f64 {
        match self {
            TypedArray::Int8(v) => v[i] as f64,
            TypedArray::Uint8(v) => v[i] as f64,
            TypedArray::Uint8Clamped(v) => v[i] as f64,
            TypedArray::Int16(v) => v[i] as f64,
            TypedArray::Uint16(v) => v[i] as f64,
            TypedArray::Int32(v) => v[i] as f64,
            TypedArray::Uint32(v) => v[i] as f64,
            TypedArray::Uint64(v) => v[i] as f64,
            TypedArray::Int64(v) => v[i] as f64,
            TypedArray::Float32(v) => v[i] as f64,
            TypedArray::Float64(v) => v[i],
        }
    }

    pub fn set_f64(&mut self, i: usize, value: f64) {
        match self {
            TypedArray::Int8(v) => v[i] = to_uint_width(value, 8) as i8,
            TypedArray::Uint8(v) => v[i] = to_uint_width(value, 8) as u8,
            TypedArray::Uint8Clamped(v) => v[i] = to_uint8_clamp(value),
            TypedArray::Int16(v) => v[i] = to_uint_width(value, 16) as i16,
            TypedArray::Uint16(v) => v[i] = to_uint_width(value, 16) as u16,
            TypedArray::Int32(v) => v[i] = to_uint_width(value, 32) as i32,
            TypedArray::Uint32(v) => v[i] = to_uint_width(value, 32) as u32,
            TypedArray::Uint64(v) => v[i] = to_uint_width(value, 64),
            TypedArray::Int64(v) => v[i] = to_uint_width(value, 64) as i64,
            TypedArray::Float32(v) => v[i] = value as f32,
            TypedArray::Float64(v) => v[i] = value,
        }
    }

    fn get_typed(&self, i: usize) -> TypedValue {
        match self {
            TypedArray::Int8(v) => TypedValue::Signed(i64::from(v[i])),
            TypedArray::Uint8(v) | TypedArray::Uint8Clamped(v) => {
                TypedValue::Unsigned(u64::from(v[i]))
            }
            TypedArray::Int16(v) => TypedValue::Signed(i64::from(v[i])),
            TypedArray::Uint16(v) => TypedValue::Unsigned(u64::from(v[i])),
            TypedArray::Int32(v) => TypedValue::Signed(i64::from(v[i])),
            TypedArray::Uint32(v) => TypedValue::Unsigned(u64::from(v[i])),
            TypedArray::Uint64(v) => TypedValue::Unsigned(v[i]),
            TypedArray::Int64(v) => TypedValue::Signed(v[i]),
            TypedArray::Float32(v) => TypedValue::Float(f64::from(v[i])),
            TypedArray::Float64(v) => TypedValue::Float(v[i]),
        }
    }

    /// Copies one element without routing integer values through `f64`.
    /// This is important for native 64-bit TIFF samples above JavaScript's
    /// safe-integer range. Integer-to-integer casts retain Rust's defined
    /// width truncation/wrapping behavior; a float destination necessarily
    /// performs the same numeric conversion as typed-array assignment.
    pub fn copy_value_from(
        &mut self,
        destination: usize,
        source: &TypedArray,
        source_index: usize,
    ) {
        let value = source.get_typed(source_index);
        macro_rules! integer_value {
            ($type:ty) => {
                match value {
                    TypedValue::Signed(value) => value as $type,
                    TypedValue::Unsigned(value) => value as $type,
                    TypedValue::Float(value) => value as $type,
                }
            };
        }
        match self {
            TypedArray::Int8(values) => values[destination] = integer_value!(i8),
            TypedArray::Uint8(values) | TypedArray::Uint8Clamped(values) => {
                values[destination] = integer_value!(u8)
            }
            TypedArray::Int16(values) => values[destination] = integer_value!(i16),
            TypedArray::Uint16(values) => values[destination] = integer_value!(u16),
            TypedArray::Int32(values) => values[destination] = integer_value!(i32),
            TypedArray::Uint32(values) => values[destination] = integer_value!(u32),
            TypedArray::Int64(values) => values[destination] = integer_value!(i64),
            TypedArray::Uint64(values) => values[destination] = integer_value!(u64),
            TypedArray::Float32(values) => {
                values[destination] = match value {
                    TypedValue::Signed(value) => value as f32,
                    TypedValue::Unsigned(value) => value as f32,
                    TypedValue::Float(value) => value as f32,
                }
            }
            TypedArray::Float64(values) => {
                values[destination] = match value {
                    TypedValue::Signed(value) => value as f64,
                    TypedValue::Unsigned(value) => value as f64,
                    TypedValue::Float(value) => value,
                }
            }
        }
    }
}

/// utils.ts `isTypedFloatArray`
pub fn is_typed_float_array(input: &TypedArray) -> bool {
    matches!(input, TypedArray::Float32(_) | TypedArray::Float64(_))
}

/// utils.ts `isTypedIntArray`
pub fn is_typed_int_array(input: &TypedArray) -> bool {
    matches!(
        input,
        TypedArray::Int8(_) | TypedArray::Int16(_) | TypedArray::Int32(_)
    )
}

/// utils.ts `isTypedUintArray`
pub fn is_typed_uint_array(input: &TypedArray) -> bool {
    matches!(
        input,
        TypedArray::Uint8(_)
            | TypedArray::Uint16(_)
            | TypedArray::Uint32(_)
            | TypedArray::Uint8Clamped(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_matches_js_groups() {
        assert!(is_typed_float_array(&TypedArray::Float32(vec![])));
        assert!(is_typed_float_array(&TypedArray::Float64(vec![])));
        assert!(!is_typed_float_array(&TypedArray::Uint8(vec![])));

        assert!(is_typed_int_array(&TypedArray::Int8(vec![])));
        assert!(is_typed_int_array(&TypedArray::Int16(vec![])));
        assert!(is_typed_int_array(&TypedArray::Int32(vec![])));
        assert!(!is_typed_int_array(&TypedArray::Uint32(vec![])));

        assert!(is_typed_uint_array(&TypedArray::Uint8(vec![])));
        assert!(is_typed_uint_array(&TypedArray::Uint8Clamped(vec![])));
        assert!(is_typed_uint_array(&TypedArray::Uint16(vec![])));
        assert!(is_typed_uint_array(&TypedArray::Uint32(vec![])));
        assert!(!is_typed_uint_array(&TypedArray::Int32(vec![])));
    }

    #[test]
    fn new_zeroed_preserves_concrete_variant() {
        let src = TypedArray::Uint16(vec![1, 2, 3]);
        let fresh = src.new_zeroed(5);
        assert_eq!(fresh, TypedArray::Uint16(vec![0; 5]));
    }

    #[test]
    fn direct_copy_preserves_integers_above_the_f64_safe_range() {
        let source = TypedArray::Uint64(vec![9_007_199_254_740_993]);
        let mut destination = TypedArray::Uint64(vec![0]);
        destination.copy_value_from(0, &source, 0);
        assert_eq!(destination, source);

        let source = TypedArray::Int64(vec![-9_007_199_254_740_993]);
        let mut destination = TypedArray::Int64(vec![0]);
        destination.copy_value_from(0, &source, 0);
        assert_eq!(destination, source);
    }

    #[test]
    fn floating_assignment_matches_ecmascript_typed_array_conversion() {
        let mut unsigned = TypedArray::Uint8(vec![0; 4]);
        for (index, value) in [-1.0, 256.0, 257.9, f64::NAN].into_iter().enumerate() {
            unsigned.set_f64(index, value);
        }
        assert_eq!(unsigned, TypedArray::Uint8(vec![255, 0, 1, 0]));

        let mut signed = TypedArray::Int8(vec![0; 2]);
        signed.set_f64(0, 255.0);
        signed.set_f64(1, 128.0);
        assert_eq!(signed, TypedArray::Int8(vec![-1, -128]));

        let mut clamped = TypedArray::Uint8Clamped(vec![0; 4]);
        for (index, value) in [-1.0, 300.0, 2.5, 3.5].into_iter().enumerate() {
            clamped.set_f64(index, value);
        }
        assert_eq!(clamped, TypedArray::Uint8Clamped(vec![0, 255, 2, 4]));
    }
}

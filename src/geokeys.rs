//! Lossless GeoKey directory representation.
//!
//! `async-tiff` exposes a convenient strongly typed GeoKey structure, but it
//! intentionally drops unknown keys and currently omits two keys understood
//! by geotiff.js (`2062` and `3096`).  The public port therefore keeps the
//! directory keyed by its numeric identifier and adds name lookup as a view;
//! no entry can disappear merely because a dependency does not know its ID.

use std::collections::BTreeMap;

/// Every GeoKey name exported by geotiff.js 3.1.0 `globals.js`.
pub const GEO_KEY_NAMES: &[(u16, &str)] = &[
    (1024, "GTModelTypeGeoKey"),
    (1025, "GTRasterTypeGeoKey"),
    (1026, "GTCitationGeoKey"),
    (2048, "GeographicTypeGeoKey"),
    (2049, "GeogCitationGeoKey"),
    (2050, "GeogGeodeticDatumGeoKey"),
    (2051, "GeogPrimeMeridianGeoKey"),
    (2052, "GeogLinearUnitsGeoKey"),
    (2053, "GeogLinearUnitSizeGeoKey"),
    (2054, "GeogAngularUnitsGeoKey"),
    (2055, "GeogAngularUnitSizeGeoKey"),
    (2056, "GeogEllipsoidGeoKey"),
    (2057, "GeogSemiMajorAxisGeoKey"),
    (2058, "GeogSemiMinorAxisGeoKey"),
    (2059, "GeogInvFlatteningGeoKey"),
    (2060, "GeogAzimuthUnitsGeoKey"),
    (2061, "GeogPrimeMeridianLongGeoKey"),
    (2062, "GeogTOWGS84GeoKey"),
    (3072, "ProjectedCSTypeGeoKey"),
    (3073, "PCSCitationGeoKey"),
    (3074, "ProjectionGeoKey"),
    (3075, "ProjCoordTransGeoKey"),
    (3076, "ProjLinearUnitsGeoKey"),
    (3077, "ProjLinearUnitSizeGeoKey"),
    (3078, "ProjStdParallel1GeoKey"),
    (3079, "ProjStdParallel2GeoKey"),
    (3080, "ProjNatOriginLongGeoKey"),
    (3081, "ProjNatOriginLatGeoKey"),
    (3082, "ProjFalseEastingGeoKey"),
    (3083, "ProjFalseNorthingGeoKey"),
    (3084, "ProjFalseOriginLongGeoKey"),
    (3085, "ProjFalseOriginLatGeoKey"),
    (3086, "ProjFalseOriginEastingGeoKey"),
    (3087, "ProjFalseOriginNorthingGeoKey"),
    (3088, "ProjCenterLongGeoKey"),
    (3089, "ProjCenterLatGeoKey"),
    (3090, "ProjCenterEastingGeoKey"),
    (3091, "ProjCenterNorthingGeoKey"),
    (3092, "ProjScaleAtNatOriginGeoKey"),
    (3093, "ProjScaleAtCenterGeoKey"),
    (3094, "ProjAzimuthAngleGeoKey"),
    (3095, "ProjStraightVertPoleLongGeoKey"),
    (3096, "ProjRectifiedGridAngleGeoKey"),
    (4096, "VerticalCSTypeGeoKey"),
    (4097, "VerticalCitationGeoKey"),
    (4098, "VerticalDatumGeoKey"),
    (4099, "VerticalUnitsGeoKey"),
];

/// Resolve a numeric GeoKey identifier to its geotiff.js property name.
pub fn geo_key_name(id: u16) -> Option<&'static str> {
    GEO_KEY_NAMES
        .binary_search_by_key(&id, |(candidate, _)| *candidate)
        .ok()
        .map(|index| GEO_KEY_NAMES[index].1)
}

/// Resolve a geotiff.js GeoKey property name to its numeric identifier.
pub fn geo_key_id(name: &str) -> Option<u16> {
    GEO_KEY_NAMES
        .iter()
        .find_map(|(id, candidate)| (*candidate == name).then_some(*id))
}

/// A value referenced by one GeoKey directory entry.
///
/// Integer variants retain their full TIFF width. This is more exact than a
/// JavaScript `number` for BigTIFF values and prevents a native transition
/// from losing precision.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedGeoKeyValue {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Ascii(String),
    UnsignedArray(Vec<u64>),
    SignedArray(Vec<i64>),
    FloatArray(Vec<f64>),
    UnsignedRational(u64, u64),
    SignedRational(i64, i64),
    UnsignedRationalArray(Vec<(u64, u64)>),
    SignedRationalArray(Vec<(i64, i64)>),
}

impl ParsedGeoKeyValue {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Unsigned(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_u16(&self) -> Option<u16> {
        self.as_u64().and_then(|value| u16::try_from(value).ok())
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Unsigned(value) => Some(*value as f64),
            Self::Signed(value) => Some(*value as f64),
            Self::Float(value) => Some(*value),
            Self::UnsignedRational(numerator, denominator) if *denominator != 0 => {
                Some(*numerator as f64 / *denominator as f64)
            }
            Self::SignedRational(numerator, denominator) if *denominator != 0 => {
                Some(*numerator as f64 / *denominator as f64)
            }
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Ascii(value) => Some(value),
            _ => None,
        }
    }
}

/// Complete GeoKey directory for one image/IFD.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeoKeys {
    entries: BTreeMap<u16, ParsedGeoKeyValue>,
}

impl GeoKeys {
    pub fn new(entries: BTreeMap<u16, ParsedGeoKeyValue>) -> Self {
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, id: u16) -> Option<&ParsedGeoKeyValue> {
        self.entries.get(&id)
    }

    pub fn get_named(&self, name: &str) -> Option<&ParsedGeoKeyValue> {
        geo_key_id(name).and_then(|id| self.get(id))
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (u16, &ParsedGeoKeyValue)> {
        self.entries.iter().map(|(&id, value)| (id, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cover_the_two_keys_missing_from_async_tiff() {
        assert_eq!(geo_key_name(2062), Some("GeogTOWGS84GeoKey"));
        assert_eq!(geo_key_name(3096), Some("ProjRectifiedGridAngleGeoKey"));
        assert_eq!(geo_key_id("ProjRectifiedGridAngleGeoKey"), Some(3096));
    }

    #[test]
    fn unknown_ids_remain_addressable_by_number() {
        let keys = GeoKeys::new(BTreeMap::from([(65_000, ParsedGeoKeyValue::Unsigned(7))]));
        assert_eq!(
            keys.get(65_000).and_then(ParsedGeoKeyValue::as_u16),
            Some(7)
        );
        assert_eq!(geo_key_name(65_000), None);
    }
}

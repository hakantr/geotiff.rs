//! Compressed byte-block cache layer sitting on top of an
//! `async_tiff::reader::AsyncFileReader`. Together, the range-reader trait
//! and this `moka`-backed cache provide geotiff.js `BlockedSource` behavior:
//! byte-weighted storage, LRU eviction, and concurrency-safe request reuse.

use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use async_tiff::reader::AsyncFileReader;
use bytes::Bytes;
use moka::future::Cache;
use moka::policy::EvictionPolicy;
use std::ops::Range;
use std::sync::Arc;

/// Default capacity for `CachedReader`'s `BlockCache`, in bytes. The 64 MiB
/// desktop default is overrideable through `GeoTiffOptions`.
pub const DEFAULT_BLOCK_CACHE_CAPACITY_BYTES: u64 = 64 * 1024 * 1024;

/// Identifies one cached byte range. `source_id` distinguishes different
/// underlying files/objects sharing one cache; `version`, if the backend
/// exposes one (e.g. an HTTP ETag), invalidates stale entries when a source
/// changes without needing an explicit eviction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RangeKey {
    pub source_id: Arc<str>,
    pub version: Option<Arc<str>>,
    pub block_offset: u64,
    pub block_length: u64,
}

/// A byte-weighted, LRU-evicting, concurrency-deduplicating cache of
/// compressed byte ranges. Capacity is in bytes, not entry count.
#[derive(Debug)]
pub struct BlockCache {
    cache: Cache<RangeKey, Bytes>,
}

impl BlockCache {
    pub fn new(max_capacity_bytes: u64) -> Self {
        let cache = Cache::builder()
            .max_capacity(max_capacity_bytes)
            .weigher(|_key: &RangeKey, value: &Bytes| -> u32 {
                value.len().try_into().unwrap_or(u32::MAX)
            })
            .eviction_policy(EvictionPolicy::lru())
            .build();
        BlockCache { cache }
    }

    /// Returns the cached block for `key`, or runs `fetch` to populate it.
    /// Concurrent calls for the same `key` are coalesced into a single
    /// `fetch` invocation (moka's `try_get_with`), so parallel tile reads
    /// that need the same underlying block don't issue duplicate I/O.
    pub async fn get_or_fetch<F, Fut, E>(&self, key: RangeKey, fetch: F) -> Result<Bytes, Arc<E>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Bytes, E>>,
        E: Send + Sync + 'static,
    {
        self.cache.try_get_with(key, fetch()).await
    }

    pub fn entry_count(&self) -> u64 {
        self.cache.entry_count()
    }

    pub fn weighted_size(&self) -> u64 {
        self.cache.weighted_size()
    }
}

/// Adapts any `AsyncFileReader` into a cached one. Wrapping happens once at
/// the dataset boundary, so every compressed tile/strip fetch benefits
/// without exposing cache concerns to raster code. Metadata discovery has
/// its own retained-prefix cache and intentionally precedes this wrapper.
#[derive(Debug)]
pub struct CachedReader {
    inner: Arc<dyn AsyncFileReader>,
    cache: Arc<BlockCache>,
    source_id: Arc<str>,
}

impl CachedReader {
    /// `source_id` only needs to be distinct from other sources sharing the
    /// *same* `BlockCache` - each `SingleGeoTiff` gets its own fresh cache
    /// (see `dataset.rs::SingleGeoTiff::open`), so a fixed constant is fine
    /// there; a shared-cache caller would pass something like a file path.
    pub fn new(
        inner: Arc<dyn AsyncFileReader>,
        cache: Arc<BlockCache>,
        source_id: impl Into<Arc<str>>,
    ) -> Self {
        CachedReader {
            inner,
            cache,
            source_id: source_id.into(),
        }
    }
}

#[async_trait::async_trait]
impl AsyncFileReader for CachedReader {
    async fn get_bytes(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
        let key = RangeKey {
            source_id: self.source_id.clone(),
            version: None,
            block_offset: range.start,
            block_length: range.end - range.start,
        };
        let inner = self.inner.clone();
        self.cache
            .get_or_fetch(key, || async move { inner.get_bytes(range).await })
            .await
            .map_err(|e| AsyncTiffError::General(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn key(offset: u64, length: u64) -> RangeKey {
        RangeKey {
            source_id: Arc::from("test-source"),
            version: None,
            block_offset: offset,
            block_length: length,
        }
    }

    #[tokio::test]
    async fn cache_hit_avoids_refetching() {
        let cache = BlockCache::new(1024 * 1024);
        let fetch_count = Arc::new(AtomicUsize::new(0));

        for _ in 0..3 {
            let fetch_count = fetch_count.clone();
            let result: Result<Bytes, Arc<std::io::Error>> = cache
                .get_or_fetch(key(0, 4), || async move {
                    fetch_count.fetch_add(1, Ordering::SeqCst);
                    Ok(Bytes::from_static(b"data"))
                })
                .await;
            assert_eq!(result.unwrap(), Bytes::from_static(b"data"));
        }

        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            1,
            "second/third get should hit the cache, not refetch"
        );
    }

    #[tokio::test]
    async fn distinct_keys_fetch_independently() {
        let cache = BlockCache::new(1024 * 1024);
        let a: Result<Bytes, Arc<std::io::Error>> = cache
            .get_or_fetch(key(0, 4), || async { Ok(Bytes::from_static(b"aaaa")) })
            .await;
        let b: Result<Bytes, Arc<std::io::Error>> = cache
            .get_or_fetch(key(4, 4), || async { Ok(Bytes::from_static(b"bbbb")) })
            .await;
        assert_eq!(a.unwrap(), Bytes::from_static(b"aaaa"));
        assert_eq!(b.unwrap(), Bytes::from_static(b"bbbb"));
        cache.cache.run_pending_tasks().await;
        assert_eq!(cache.entry_count(), 2);
    }

    #[tokio::test]
    async fn fetch_errors_are_not_cached_as_values() {
        let cache = BlockCache::new(1024 * 1024);
        let result = cache
            .get_or_fetch(key(0, 4), || async {
                Err::<Bytes, _>(std::io::Error::other("boom"))
            })
            .await;
        assert!(result.is_err());
    }

    #[derive(Debug)]
    struct CountingReader {
        data: Bytes,
        fetch_count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl AsyncFileReader for CountingReader {
        async fn get_bytes(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
            self.fetch_count.fetch_add(1, Ordering::SeqCst);
            let end = (range.end as usize).min(self.data.len());
            Ok(self.data.slice(range.start as usize..end))
        }
    }

    #[tokio::test]
    async fn cached_reader_avoids_refetching_the_same_range() {
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let inner: Arc<dyn AsyncFileReader> = Arc::new(CountingReader {
            data: Bytes::from_static(b"0123456789"),
            fetch_count: fetch_count.clone(),
        });
        let cache = Arc::new(BlockCache::new(1024 * 1024));
        let cached = CachedReader::new(inner, cache, "test");

        for _ in 0..3 {
            let bytes = cached.get_bytes(2..5).await.unwrap();
            assert_eq!(bytes.as_ref(), b"234");
        }

        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            1,
            "repeated identical-range reads should hit the cache, not the inner reader"
        );
    }

    #[tokio::test]
    async fn cached_reader_fetches_distinct_ranges_independently() {
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let inner: Arc<dyn AsyncFileReader> = Arc::new(CountingReader {
            data: Bytes::from_static(b"0123456789"),
            fetch_count: fetch_count.clone(),
        });
        let cache = Arc::new(BlockCache::new(1024 * 1024));
        let cached = CachedReader::new(inner, cache, "test");

        assert_eq!(cached.get_bytes(0..2).await.unwrap().as_ref(), b"01");
        assert_eq!(cached.get_bytes(2..4).await.unwrap().as_ref(), b"23");

        assert_eq!(fetch_count.load(Ordering::SeqCst), 2);
    }
}

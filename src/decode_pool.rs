//! Dedicated, fixed-size Rayon pool for TIFF decode work. It is separate
//! from Rayon's process-wide default pool, so codec work neither contends
//! with unrelated Rayon users nor blocks Tokio/GPUI threads. `pipeline.rs`
//! and `block.rs` only reach it through async `spawn_decode` calls.
//!
//! Sized once, lazily, from `std::thread::available_parallelism()` - the
//! number of logical CPUs the OS reports as available to this process -
//! computed the first time any decode is requested and cached in a
//! `OnceLock` for the rest of the process's lifetime, rather than
//! hardcoding a thread count or recomputing it per call. Overridable via
//! `configure_decode_pool` if called early enough (see its doc comment).
//!
//! A `Semaphore` bounds queued/running decodes so concurrent tile requests
//! cannot accumulate unbounded captured data. `CancellationToken` is a
//! cooperative flag checked before submission and after completion; Rayon
//! cannot preempt a codec already running.

use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::{Notify, Semaphore};

static DECODE_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

fn build_pool(num_threads: Option<usize>) -> rayon::ThreadPool {
    let mut builder =
        rayon::ThreadPoolBuilder::new().thread_name(|i| format!("geotiff-decode-{i}"));
    // A zero configuration asks Rayon to use its automatic thread count.
    // geotiff.js `new Pool(0)` instead decodes inline; the native spelling
    // for that path is calling the public decoder returned by `get_decoder`
    // directly. `configure_decode_pool` configures only the worker-pool path.
    if let Some(n) = num_threads.filter(|value| *value != 0) {
        builder = builder.num_threads(n);
    }
    // No explicit `num_threads` call when neither `configure_decode_pool`
    // nor `available_parallelism` gave us a number (the latter fails only
    // in unusual sandboxed environments) - `build()` then falls back to
    // Rayon's own default sizing, which tries the same query internally
    // with its own fallback, rather than us guessing a number.
    builder
        .build()
        .expect("failed to build the dedicated geotiff decode thread pool")
}

fn decode_pool() -> &'static rayon::ThreadPool {
    DECODE_POOL.get_or_init(|| {
        build_pool(
            std::thread::available_parallelism()
                .ok()
                .map(std::num::NonZeroUsize::get),
        )
    })
}

/// Overrides the decode pool's thread count. Only takes effect if called
/// *before* anything has
/// triggered the pool's lazy build (i.e. before the first `spawn_decode`
/// call anywhere in the process) - call this during app startup if the
/// `available_parallelism()`-derived default isn't what you want. Returns
/// `Err(num_threads)` (the requested count, unused) if the pool was
/// already built.
pub fn configure_decode_pool(num_threads: usize) -> Result<(), usize> {
    DECODE_POOL
        .set(build_pool(Some(num_threads)))
        .map_err(|_| num_threads)
}

/// Caps in-flight (queued-or-running) decodes to the pool's own thread
/// count - beyond that, a `spawn_decode` caller waits on the semaphore
/// (an async wait, not a blocked Tokio worker) instead of piling more
/// captured tile/strip data into Rayon's work queue. Sized from the same
/// `decode_pool()`, so it's always consistent with however many threads
/// actually got built (real hardware value, or Rayon's own fallback).
static DECODE_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

fn decode_semaphore() -> &'static Semaphore {
    DECODE_SEMAPHORE.get_or_init(|| Semaphore::new(decode_pool().current_num_threads()))
}

/// A simple, non-hierarchical cancellation flag, cheap to `Clone` (an
/// `Arc<AtomicBool>` underneath) so it can be threaded through the read
/// pipeline and held by whoever wants to cancel a read (e.g. a GPUI tile
/// request the user has already panned away from). See the module doc for
/// why this only gates "don't start"/"don't use the result", not
/// mid-decode interruption.
#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

#[derive(Debug, Clone)]
pub struct CancellationToken(Arc<CancellationState>);

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        CancellationToken(Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        }))
    }

    pub fn cancel(&self) {
        if !self.0.cancelled.swap(true, Ordering::AcqRel) {
            self.0.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    /// Resolves when cancellation is requested. The pre-check before
    /// registering with `Notify` and the second check after registration
    /// avoid missed wakeups when `cancel()` races this method.
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.0.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

fn cancelled_err() -> AsyncTiffError {
    AsyncTiffError::General("decode cancelled".to_string())
}

/// For callers that stitch several decodes together in a loop (`raster.rs`'s
/// tile/strip loops): bail out of the loop early once cancellation is
/// noticed, instead of fetching/decoding tiles nobody will use. `None`
/// (no cancellation support requested) always passes.
pub fn check_cancelled(cancellation: Option<&CancellationToken>) -> AsyncTiffResult<()> {
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Err(cancelled_err());
    }
    Ok(())
}

/// Await an I/O operation while honoring a cancellation token. Dropping the
/// losing future cancels reqwest/tokio range reads instead of merely ignoring
/// their result after all bytes have arrived.
pub async fn cancellable<F, T>(
    future: F,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<T>
where
    F: Future<Output = AsyncTiffResult<T>>,
{
    check_cancelled(cancellation)?;
    let Some(token) = cancellation else {
        return future.await;
    };
    tokio::select! {
        result = future => result,
        () = token.cancelled() => Err(cancelled_err()),
    }
}

/// Runs `f` on the dedicated decode pool and awaits its result from the
/// calling Tokio task, bridging the two runtimes via a `oneshot` channel.
/// `f` is expected to do only CPU-bound work (decompression, predictor
/// application, sample conversion) - I/O (`AsyncFileReader::get_bytes`)
/// should already be done and its bytes captured before calling this.
///
/// `cancellation`, when given, is checked twice: before acquiring a
/// semaphore permit/submitting to the pool (skip entirely if already
/// cancelled - don't even queue), and after the pool work completes
/// (discard the result if cancellation happened while it was running).
/// `None` means "no cancellation support requested", not "already
/// cancelled" - existing callers that don't care about this keep working
/// unchanged.
pub async fn spawn_decode<F, T>(
    f: F,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<T>
where
    F: FnOnce() -> AsyncTiffResult<T> + Send + 'static,
    T: Send + 'static,
{
    check_cancelled(cancellation)?;

    let _permit = decode_semaphore()
        .acquire()
        .await
        .expect("decode semaphore is never closed");

    // Cancellation may have happened while this job was queued behind the
    // semaphore. Never submit cancelled work to Rayon once a permit becomes
    // available.
    check_cancelled(cancellation)?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    decode_pool().spawn(move || {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|payload| {
                let message = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown decoder panic");
                Err(AsyncTiffError::General(format!(
                    "decode task panicked: {message}"
                )))
            });
        let _ = tx.send(result);
    });
    let result = rx
        .await
        .unwrap_or_else(|_| Err(AsyncTiffError::General("decode task panicked".to_string())));

    if check_cancelled(cancellation).is_err() {
        return Err(cancelled_err());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_decode_runs_the_closure_and_returns_its_result() {
        let result = spawn_decode(|| Ok(2 + 2), None).await;
        assert_eq!(result.unwrap(), 4);
    }

    #[tokio::test]
    async fn spawn_decode_propagates_the_closures_error() {
        let result: AsyncTiffResult<()> =
            spawn_decode(|| Err(AsyncTiffError::General("boom".to_string())), None).await;
        assert!(result.unwrap_err().to_string().contains("boom"));
    }

    #[tokio::test]
    async fn spawn_decode_converts_panics_to_errors_without_killing_the_pool() {
        let result: AsyncTiffResult<()> = spawn_decode(|| panic!("bad codec input"), None).await;
        assert!(result.unwrap_err().to_string().contains("bad codec input"));
        assert_eq!(spawn_decode(|| Ok(42), None).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn many_concurrent_spawns_all_complete() {
        // Deliberately more than any realistic thread count, to also
        // exercise the semaphore's queuing (not just the pool's).
        let handles: Vec<_> = (0..256)
            .map(|i| tokio::spawn(spawn_decode(move || Ok::<_, AsyncTiffError>(i * 2), None)))
            .collect();
        for (i, handle) in handles.into_iter().enumerate() {
            assert_eq!(handle.await.unwrap().unwrap(), i * 2);
        }
    }

    #[test]
    fn decode_pool_has_at_least_one_thread() {
        assert!(decode_pool().current_num_threads() >= 1);
    }

    #[tokio::test]
    async fn a_token_cancelled_before_the_call_skips_the_work_entirely() {
        let token = CancellationToken::new();
        token.cancel();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_clone = ran.clone();
        let result: AsyncTiffResult<()> = spawn_decode(
            move || {
                ran_clone.store(true, Ordering::SeqCst);
                Ok(())
            },
            Some(&token),
        )
        .await;
        assert!(result.is_err());
        assert!(
            !ran.load(Ordering::SeqCst),
            "the closure must not run once the token is already cancelled"
        );
    }

    #[tokio::test]
    async fn a_token_cancelled_while_running_discards_the_result() {
        let token = CancellationToken::new();
        let token_clone = token.clone();
        // Cancels from inside the closure (simulating cancellation racing
        // with in-flight work) - the closure itself always finishes
        // (Rayon has no preemption), but the caller must see an error.
        let result = spawn_decode(
            move || {
                token_clone.cancel();
                Ok::<_, AsyncTiffError>(42)
            },
            Some(&token),
        )
        .await;
        assert!(
            result.is_err(),
            "a token cancelled mid-decode must discard the (already-computed) result"
        );
    }

    #[tokio::test]
    async fn an_uncancelled_token_behaves_like_no_token_at_all() {
        let token = CancellationToken::new();
        let result = spawn_decode(|| Ok(7), Some(&token)).await;
        assert_eq!(result.unwrap(), 7);
    }

    #[test]
    fn cancellation_token_starts_uncancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        // Clones share the same underlying flag.
        assert!(clone.is_cancelled());
    }

    #[test]
    fn default_token_is_uncancelled() {
        assert!(!CancellationToken::default().is_cancelled());
    }

    #[test]
    fn configure_decode_pool_only_takes_effect_once() {
        // `DECODE_POOL` is process-global and shared with every other test
        // in this binary, so whether the *first* call here wins depends on
        // test execution order - but the *second* call must always lose,
        // regardless of order, since by then the pool definitely exists
        // (either from this test's own first call, or from some earlier
        // test having already triggered the lazy build).
        let _first = configure_decode_pool(3);
        let second = configure_decode_pool(3);
        assert!(
            second.is_err(),
            "a call after the pool already exists must never succeed"
        );
        assert_eq!(second.unwrap_err(), 3);
    }
}

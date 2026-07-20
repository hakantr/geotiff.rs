//! Port of `logging.js`. JS's `...args: unknown[]` variadic logging calls
//! have no Rust equivalent; callers format their own message (`format!(...)`)
//! before calling, which is the idiomatic Rust shape for a pluggable logger
//! and preserves the actual behavior (delegate-or-no-op) exactly.

use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub trait Logger: Send + Sync {
    fn log(&self, message: &str);
    fn debug(&self, message: &str);
    fn info(&self, message: &str);
    fn warn(&self, message: &str);
    fn error(&self, message: &str);
    fn time(&self, label: &str);
    fn time_end(&self, label: &str);
}

/// `class DummyLogger` - a no-op logger.
pub struct DummyLogger;

impl Logger for DummyLogger {
    fn log(&self, _message: &str) {}
    fn debug(&self, _message: &str) {}
    fn info(&self, _message: &str) {}
    fn warn(&self, _message: &str) {}
    fn error(&self, _message: &str) {}
    fn time(&self, _label: &str) {}
    fn time_end(&self, _label: &str) {}
}

static LOGGER: OnceLock<RwLock<Box<dyn Logger>>> = OnceLock::new();

fn logger() -> &'static RwLock<Box<dyn Logger>> {
    LOGGER.get_or_init(|| RwLock::new(Box::new(DummyLogger)))
}

fn read_logger() -> RwLockReadGuard<'static, Box<dyn Logger>> {
    logger()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_logger() -> RwLockWriteGuard<'static, Box<dyn Logger>> {
    logger()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `setLogger(logger = new DummyLogger())`
pub fn set_logger(logger_impl: Box<dyn Logger>) {
    *write_logger() = logger_impl;
}

pub fn debug(message: &str) {
    read_logger().debug(message);
}

pub fn log(message: &str) {
    read_logger().log(message);
}

pub fn info(message: &str) {
    read_logger().info(message);
}

pub fn warn(message: &str) {
    read_logger().warn(message);
}

pub fn error(message: &str) {
    read_logger().error(message);
}

pub fn time(label: &str) {
    read_logger().time(label);
}

pub fn time_end(label: &str) {
    read_logger().time_end(label);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingLogger {
        count: Arc<AtomicUsize>,
    }

    impl Logger for CountingLogger {
        fn log(&self, _message: &str) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
        fn debug(&self, _message: &str) {}
        fn info(&self, _message: &str) {}
        fn warn(&self, _message: &str) {}
        fn error(&self, _message: &str) {}
        fn time(&self, _label: &str) {}
        fn time_end(&self, _label: &str) {}
    }

    #[test]
    fn dummy_logger_is_a_no_op() {
        // exercised only for panics - a no-op has no observable state to assert on
        let l = DummyLogger;
        l.log("x");
        l.debug("x");
        l.info("x");
        l.warn("x");
        l.error("x");
        l.time("x");
        l.time_end("x");
    }

    #[test]
    fn set_logger_replaces_the_global_logger() {
        let count = Arc::new(AtomicUsize::new(0));
        set_logger(Box::new(CountingLogger {
            count: count.clone(),
        }));
        log("hello");
        log("world");
        assert_eq!(count.load(Ordering::SeqCst), 2);
        // leave a DummyLogger installed so other tests in this process aren't affected
        set_logger(Box::new(DummyLogger));
    }
}

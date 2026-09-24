use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use uuid::Uuid;

const NOTIFICATIONS_PER_MINUTE: f64 = 60.0;
const NOTIFICATIONS_BURST: f64 = 20.0;
const STATUS_PER_MINUTE: f64 = 120.0;
const STATUS_BURST: f64 = 40.0;
const CHANNEL_MUTATIONS_PER_MINUTE: f64 = 10.0;
const CHANNEL_MUTATIONS_BURST: f64 = 5.0;
const WECHAT_ADMIN_GRANTS_PER_MINUTE: f64 = 10.0;
const WECHAT_ADMIN_GRANTS_BURST: f64 = 5.0;

/// The independently metered local API operations.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum RateLimitOperation {
    Notifications,
    Status,
    ChannelMutations,
    WechatAdminGrants,
}

impl RateLimitOperation {
    fn policy(self) -> Policy {
        match self {
            Self::Notifications => {
                Policy::per_minute(NOTIFICATIONS_PER_MINUTE, NOTIFICATIONS_BURST)
            }
            Self::Status => Policy::per_minute(STATUS_PER_MINUTE, STATUS_BURST),
            Self::ChannelMutations => {
                Policy::per_minute(CHANNEL_MUTATIONS_PER_MINUTE, CHANNEL_MUTATIONS_BURST)
            }
            Self::WechatAdminGrants => {
                Policy::per_minute(WECHAT_ADMIN_GRANTS_PER_MINUTE, WECHAT_ADMIN_GRANTS_BURST)
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Policy {
    tokens_per_second: f64,
    capacity: f64,
}

impl Policy {
    const fn per_minute(tokens: f64, capacity: f64) -> Self {
        Self {
            tokens_per_second: tokens / 60.0,
            capacity,
        }
    }
}

/// A monotonic time source. The production implementation is process-local,
/// while tests can inject a deterministic clock without changing wall time.
pub trait MonotonicClock: Send + Sync + 'static {
    fn now(&self) -> Duration;
}

struct SystemMonotonicClock {
    origin: Instant,
}

impl SystemMonotonicClock {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl MonotonicClock for SystemMonotonicClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimitExceeded {
    retry_after: Duration,
}

impl RateLimitExceeded {
    pub fn retry_after(self) -> Duration {
        self.retry_after
    }
}

/// A process-local, per-device token bucket. Clones share the same buckets;
/// constructing a new limiter starts with an empty in-memory state.
#[derive(Clone)]
pub struct LocalRateLimiter {
    inner: Arc<Inner>,
}

struct Inner {
    buckets: Mutex<HashMap<(Uuid, RateLimitOperation), Bucket>>,
    clock: Arc<dyn MonotonicClock>,
}

impl fmt::Debug for LocalRateLimiter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalRateLimiter")
            .finish_non_exhaustive()
    }
}

impl Default for LocalRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalRateLimiter {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemMonotonicClock::new()))
    }

    pub fn with_clock(clock: Arc<dyn MonotonicClock>) -> Self {
        Self {
            inner: Arc::new(Inner {
                buckets: Mutex::new(HashMap::new()),
                clock,
            }),
        }
    }

    /// Consumes one token for the device and operation, or returns the
    /// monotonic delay until another token becomes available.
    pub fn check(
        &self,
        device_id: Uuid,
        operation: RateLimitOperation,
    ) -> Result<(), RateLimitExceeded> {
        let now = self.inner.clock.now();
        let policy = operation.policy();
        let mut buckets = self.lock_buckets();
        let bucket = buckets
            .entry((device_id, operation))
            .or_insert_with(|| Bucket::full(policy, now));
        bucket.consume(policy, now)
    }

    fn lock_buckets(&self) -> MutexGuard<'_, HashMap<(Uuid, RateLimitOperation), Bucket>> {
        self.inner
            .buckets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct Bucket {
    tokens: f64,
    last_refill: Duration,
}

impl Bucket {
    fn full(policy: Policy, now: Duration) -> Self {
        Self {
            tokens: policy.capacity,
            last_refill: now,
        }
    }

    fn consume(&mut self, policy: Policy, now: Duration) -> Result<(), RateLimitExceeded> {
        let elapsed = now.saturating_sub(self.last_refill);
        self.tokens =
            (self.tokens + elapsed.as_secs_f64() * policy.tokens_per_second).min(policy.capacity);
        self.last_refill = self.last_refill.max(now);

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }

        let missing = 1.0 - self.tokens;
        Err(RateLimitExceeded {
            retry_after: Duration::from_secs_f64(missing / policy.tokens_per_second),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Barrier,
        atomic::{AtomicU64, Ordering},
    };

    use super::*;

    #[derive(Default)]
    struct ManualClock {
        nanos: AtomicU64,
    }

    impl ManualClock {
        fn advance(&self, duration: Duration) {
            self.nanos.fetch_add(
                u64::try_from(duration.as_nanos()).expect("test duration fits in u64"),
                Ordering::SeqCst,
            );
        }
    }

    impl MonotonicClock for ManualClock {
        fn now(&self) -> Duration {
            Duration::from_nanos(self.nanos.load(Ordering::SeqCst))
        }
    }

    fn limiter() -> (LocalRateLimiter, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::default());
        let limiter = LocalRateLimiter::with_clock(clock.clone());
        (limiter, clock)
    }

    #[test]
    fn notifications_allow_burst_twenty_and_refill_at_sixty_per_minute() {
        let (limiter, clock) = limiter();
        let device = Uuid::new_v4();

        for _ in 0..20 {
            assert!(
                limiter
                    .check(device, RateLimitOperation::Notifications)
                    .is_ok()
            );
        }
        assert_eq!(
            limiter
                .check(device, RateLimitOperation::Notifications)
                .expect_err("burst is exhausted")
                .retry_after(),
            Duration::from_secs(1)
        );

        clock.advance(Duration::from_secs(1));
        assert!(
            limiter
                .check(device, RateLimitOperation::Notifications)
                .is_ok()
        );
    }

    #[test]
    fn status_allows_burst_forty_and_refills_at_one_token_per_half_second() {
        let (limiter, clock) = limiter();
        let device = Uuid::new_v4();

        for _ in 0..40 {
            assert!(limiter.check(device, RateLimitOperation::Status).is_ok());
        }
        assert_eq!(
            limiter
                .check(device, RateLimitOperation::Status)
                .expect_err("burst is exhausted")
                .retry_after(),
            Duration::from_millis(500)
        );

        clock.advance(Duration::from_millis(500));
        assert!(limiter.check(device, RateLimitOperation::Status).is_ok());
    }

    #[test]
    fn all_channel_mutations_share_one_bucket() {
        let (limiter, clock) = limiter();
        let device = Uuid::new_v4();
        for _ in 0..5 {
            limiter
                .check(device, RateLimitOperation::ChannelMutations)
                .expect("shared burst");
        }
        assert_eq!(
            limiter
                .check(device, RateLimitOperation::ChannelMutations)
                .expect_err("exhausted")
                .retry_after(),
            Duration::from_secs(6)
        );
        clock.advance(Duration::from_secs(6));
        assert!(
            limiter
                .check(device, RateLimitOperation::ChannelMutations)
                .is_ok()
        );
    }

    #[test]
    fn devices_and_operations_have_independent_buckets() {
        let (limiter, _) = limiter();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        for _ in 0..20 {
            limiter
                .check(first, RateLimitOperation::Notifications)
                .expect("first device notification burst");
        }

        assert!(
            limiter
                .check(first, RateLimitOperation::Notifications)
                .is_err()
        );
        assert!(
            limiter
                .check(second, RateLimitOperation::Notifications)
                .is_ok()
        );
        assert!(limiter.check(first, RateLimitOperation::Status).is_ok());
    }

    #[test]
    fn concurrent_checks_cannot_exceed_the_shared_burst() {
        let (limiter, _) = limiter();
        let device = Uuid::new_v4();
        let barrier = Arc::new(Barrier::new(65));
        let mut workers = Vec::new();

        for _ in 0..64 {
            let limiter = limiter.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                limiter
                    .check(device, RateLimitOperation::Notifications)
                    .is_ok()
            }));
        }
        barrier.wait();

        let allowed: usize = workers
            .into_iter()
            .map(|worker| usize::from(worker.join().expect("worker did not panic")))
            .sum();
        assert_eq!(allowed, 20);
    }

    #[test]
    fn a_new_limiter_has_no_memory_of_previous_buckets() {
        let clock = Arc::new(ManualClock::default());
        let device = Uuid::new_v4();
        let first = LocalRateLimiter::with_clock(clock.clone());
        for _ in 0..20 {
            first
                .check(device, RateLimitOperation::Notifications)
                .expect("initial burst");
        }
        assert!(
            first
                .check(device, RateLimitOperation::Notifications)
                .is_err()
        );

        let restarted = LocalRateLimiter::with_clock(clock);
        assert!(
            restarted
                .check(device, RateLimitOperation::Notifications)
                .is_ok()
        );
    }
}

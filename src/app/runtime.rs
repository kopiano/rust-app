use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::sync::Semaphore;

const DEFAULT_HTTP_CONCURRENCY: usize = 128;
const DEFAULT_UPLOAD_CONCURRENCY: usize = 4;
const DEFAULT_BCRYPT_CONCURRENCY: usize = 4;
const DEFAULT_TRANSCODE_CONCURRENCY: usize = 2;
const DEFAULT_HTTP_QUEUE_TIMEOUT_MS: u64 = 250;
const DEFAULT_UPLOAD_QUEUE_TIMEOUT_MS: u64 = 1_000;
const DEFAULT_BCRYPT_QUEUE_TIMEOUT_MS: u64 = 500;

pub struct RuntimeLimits {
    pub http: Arc<Semaphore>,
    pub upload: Arc<Semaphore>,
    pub bcrypt: Arc<Semaphore>,
    pub transcode: Arc<Semaphore>,
    pub http_queue_timeout: Duration,
    pub upload_queue_timeout: Duration,
    pub bcrypt_queue_timeout: Duration,
    pub http_max: usize,
    pub upload_max: usize,
    pub bcrypt_max: usize,
    pub transcode_max: usize,
}

impl RuntimeLimits {
    pub fn from_env() -> Self {
        let http_max = env_usize("HTTP_CONCURRENCY", DEFAULT_HTTP_CONCURRENCY);
        let upload_max = env_usize("UPLOAD_CONCURRENCY", DEFAULT_UPLOAD_CONCURRENCY);
        let bcrypt_max = env_usize("BCRYPT_CONCURRENCY", DEFAULT_BCRYPT_CONCURRENCY);
        let transcode_max = env_usize("TRANSCODE_CONCURRENCY", DEFAULT_TRANSCODE_CONCURRENCY);
        Self {
            http: Arc::new(Semaphore::new(http_max)),
            upload: Arc::new(Semaphore::new(upload_max)),
            bcrypt: Arc::new(Semaphore::new(bcrypt_max)),
            transcode: Arc::new(Semaphore::new(transcode_max)),
            http_queue_timeout: Duration::from_millis(env_u64(
                "HTTP_QUEUE_TIMEOUT_MS",
                DEFAULT_HTTP_QUEUE_TIMEOUT_MS,
            )),
            upload_queue_timeout: Duration::from_millis(env_u64(
                "UPLOAD_QUEUE_TIMEOUT_MS",
                DEFAULT_UPLOAD_QUEUE_TIMEOUT_MS,
            )),
            bcrypt_queue_timeout: Duration::from_millis(env_u64(
                "BCRYPT_QUEUE_TIMEOUT_MS",
                DEFAULT_BCRYPT_QUEUE_TIMEOUT_MS,
            )),
            http_max,
            upload_max,
            bcrypt_max,
            transcode_max,
        }
    }
}

pub struct AppMetrics {
    started_at: Instant,
    pub http_total: AtomicU64,
    pub http_active: AtomicUsize,
    pub http_rejected: AtomicU64,
    pub upload_active: AtomicUsize,
    pub upload_rejected: AtomicU64,
    pub bcrypt_active: AtomicUsize,
    pub bcrypt_rejected: AtomicU64,
}

impl Default for AppMetrics {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            http_total: AtomicU64::new(0),
            http_active: AtomicUsize::new(0),
            http_rejected: AtomicU64::new(0),
            upload_active: AtomicUsize::new(0),
            upload_rejected: AtomicU64::new(0),
            bcrypt_active: AtomicUsize::new(0),
            bcrypt_rejected: AtomicU64::new(0),
        }
    }
}

impl AppMetrics {
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            uptime_seconds: self.started_at.elapsed().as_secs(),
            http_total: self.http_total.load(Ordering::Relaxed),
            http_active: self.http_active.load(Ordering::Relaxed),
            http_rejected: self.http_rejected.load(Ordering::Relaxed),
            upload_active: self.upload_active.load(Ordering::Relaxed),
            upload_rejected: self.upload_rejected.load(Ordering::Relaxed),
            bcrypt_active: self.bcrypt_active.load(Ordering::Relaxed),
            bcrypt_rejected: self.bcrypt_rejected.load(Ordering::Relaxed),
        }
    }
}

#[derive(Serialize)]
pub struct MetricsSnapshot {
    pub uptime_seconds: u64,
    pub http_total: u64,
    pub http_active: usize,
    pub http_rejected: u64,
    pub upload_active: usize,
    pub upload_rejected: u64,
    pub bcrypt_active: usize,
    pub bcrypt_rejected: u64,
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

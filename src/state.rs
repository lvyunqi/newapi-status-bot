use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::api::PerfMetricData;
use crate::config::AppConfig;
use crate::metrics::WindowSnapshot;
use crate::repository::DatabaseStats;

static GLOBAL_STATE: OnceLock<Mutex<Option<Arc<AppState>>>> = OnceLock::new();

/// 采集器对命令暴露的最小健康信息。
#[derive(Debug, Clone, Default)]
pub struct CollectorHealth {
    pub started_at: i64,
    pub last_attempt_at: i64,
    pub last_success_at: i64,
    pub consecutive_failures: u32,
    pub last_error: String,
}

#[derive(Debug, Clone, Default)]
pub struct ReportCache {
    pub generated_at: i64,
    pub windows: HashMap<i64, WindowSnapshot>,
    pub database: DatabaseStats,
}

#[derive(Debug, Default)]
pub struct PushState {
    pub last_heartbeat_at: i64,
    pub last_push_at: i64,
    pub last_sent_fingerprint: String,
    pub last_sent_had_alert: bool,
    pub candidate_fingerprint: String,
    pub candidate_count: u32,
}

#[derive(Debug, Clone)]
pub struct PerfCacheEntry {
    pub fetched_at: i64,
    pub data: PerfMetricData,
}

#[derive(Debug, Default)]
struct ControlFlags {
    stop: bool,
    wake: bool,
}

/// 用条件变量打断轮询等待，确保刷新和热卸载不需要等待完整周期。
#[derive(Debug, Default)]
pub struct WorkerControl {
    flags: Mutex<ControlFlags>,
    changed: Condvar,
}

impl WorkerControl {
    pub fn request_refresh(&self) {
        if let Ok(mut flags) = self.flags.lock() {
            flags.wake = true;
            self.changed.notify_all();
        }
    }

    pub fn request_stop(&self) {
        if let Ok(mut flags) = self.flags.lock() {
            flags.stop = true;
            self.changed.notify_all();
        }
    }

    /// 返回 true 表示线程应退出，false 表示继续下一轮采集。
    pub fn wait(&self, duration: Duration) -> bool {
        let Ok(flags) = self.flags.lock() else {
            return true;
        };
        if flags.stop {
            return true;
        }
        let Ok((mut flags, _)) = self.changed.wait_timeout(flags, duration) else {
            return true;
        };
        if flags.stop {
            return true;
        }
        flags.wake = false;
        false
    }
}

pub struct AppState {
    pub config: AppConfig,
    pub database_path: PathBuf,
    pub health: RwLock<CollectorHealth>,
    pub reports: RwLock<ReportCache>,
    pub control: Arc<WorkerControl>,
    pub push: Mutex<PushState>,
    pub perf_cache: Mutex<HashMap<(String, u32), PerfCacheEntry>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl AppState {
    pub fn new(config: AppConfig, database_path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            config,
            database_path,
            health: RwLock::new(CollectorHealth {
                started_at: unix_now(),
                ..CollectorHealth::default()
            }),
            reports: RwLock::new(ReportCache::default()),
            control: Arc::new(WorkerControl::default()),
            push: Mutex::new(PushState::default()),
            perf_cache: Mutex::new(HashMap::new()),
            worker: Mutex::new(None),
        })
    }

    pub fn start_worker(self: &Arc<Self>) -> Result<(), String> {
        let mut slot = self
            .worker
            .lock()
            .map_err(|_| "采集线程状态锁已损坏".to_string())?;
        if slot.is_some() {
            return Err("采集线程已经启动".to_string());
        }
        let state = Arc::clone(self);
        let handle = thread::Builder::new()
            .name("newapi-status-collector".to_string())
            .spawn(move || crate::collector::run(state))
            .map_err(|error| format!("启动采集线程失败: {error}"))?;
        *slot = Some(handle);
        Ok(())
    }

    pub fn shutdown(&self) {
        self.control.request_stop();
        let handle = self.worker.lock().ok().and_then(|mut slot| slot.take());
        if let Some(handle) = handle
            && handle.join().is_err()
        {
            eprintln!("[newapi-status-bot] collector thread panicked during shutdown");
        }
    }
}

pub fn install(state: Arc<AppState>) -> Result<(), String> {
    let container = GLOBAL_STATE.get_or_init(|| Mutex::new(None));
    let mut current = container
        .lock()
        .map_err(|_| "全局插件状态锁已损坏".to_string())?;
    if current.is_some() {
        return Err("插件已经初始化".to_string());
    }
    *current = Some(state);
    Ok(())
}

pub fn current() -> Option<Arc<AppState>> {
    GLOBAL_STATE
        .get()
        .and_then(|container| container.lock().ok())
        .and_then(|state| state.as_ref().map(Arc::clone))
}

pub fn take() -> Option<Arc<AppState>> {
    GLOBAL_STATE
        .get()
        .and_then(|container| container.lock().ok())
        .and_then(|mut state| state.take())
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_interrupts_wait_without_stopping() {
        let control = Arc::new(WorkerControl::default());
        control.request_refresh();
        assert!(!control.wait(Duration::from_millis(1)));
    }

    #[test]
    fn stop_interrupts_wait() {
        let control = WorkerControl::default();
        control.request_stop();
        assert!(control.wait(Duration::from_secs(1)));
    }
}

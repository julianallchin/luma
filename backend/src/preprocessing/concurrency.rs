//! How many tracks' analysis may run at once, and the one way to run a batch
//! under that bound.

use crate::preprocessing::AnalysisGuard;

/// The one concurrency knob for per-track background work: how many tracks may
/// be in flight at once, derived from available system memory (reserve 4 GB for
/// the OS/app, ~3 GB per worker for stems + beats).
///
/// Every fan-out over a track batch — the preprocessing DAG and the waveform
/// branch beside it — bounds itself by this number. That is not only a memory
/// budget: each in-flight track holds a SQLite connection while it writes its
/// artifacts, so a batch wider than the pool starves it and the writes fail
/// with `pool timed out` rather than doing less work more slowly.
///
/// Each branch takes its own permits, so a batch has up to twice this many
/// tracks in flight; the cap of 6 keeps that inside the app pool's 16
/// connections. One shared bound would queue the cheap waveform jobs behind
/// stem separation.
///
/// Memoized, so the number cannot differ between two branches of the same
/// import and the line is logged once.
pub fn analysis_worker_count() -> usize {
    static WORKERS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *WORKERS.get_or_init(|| {
        let ram_gb = total_system_memory_gb();
        let workers = ((ram_gb as i64 - 4) / 3).clamp(1, 6) as usize;
        eprintln!("[background_analysis] {ram_gb} GB RAM → {workers} parallel workers");
        workers
    })
}

#[cfg(target_os = "macos")]
fn total_system_memory_gb() -> u64 {
    use std::mem;
    let mut memsize: u64 = 0;
    let mut size = mem::size_of::<u64>();
    let mut mib = [libc::CTL_HW, libc::HW_MEMSIZE];
    let ret = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            &mut memsize as *mut u64 as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 {
        memsize / (1024 * 1024 * 1024)
    } else {
        8 // conservative fallback
    }
}

#[cfg(target_os = "linux")]
fn total_system_memory_gb() -> u64 {
    use std::fs;
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
                .map(|kb| kb / (1024 * 1024))
        })
        .unwrap_or(8)
}

#[cfg(target_os = "windows")]
fn total_system_memory_gb() -> u64 {
    use std::mem;
    #[repr(C)]
    struct MemoryStatusEx {
        dw_length: u32,
        dw_memory_load: u32,
        ull_total_phys: u64,
        ull_avail_phys: u64,
        ull_total_page_file: u64,
        ull_avail_page_file: u64,
        ull_total_virtual: u64,
        ull_avail_virtual: u64,
        ull_avail_extended_virtual: u64,
    }
    extern "system" {
        fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut status: MemoryStatusEx = unsafe { mem::zeroed() };
    status.dw_length = mem::size_of::<MemoryStatusEx>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 {
        status.ull_total_phys / (1024 * 1024 * 1024)
    } else {
        8
    }
}

/// Run one job per track with at most `limit` in flight, stopping early when
/// the analysis guard closes.
///
/// `limit` is a *resource* bound, not a tuning preference — see
/// [`analysis_worker_count`]. A permit is held for the whole job, including the
/// database reads it opens with, so nothing in a batch can be wider than the
/// number of tracks the pool can serve.
pub async fn run_bounded_track_jobs<Job, Fut>(
    track_ids: &[String],
    analysis: &AnalysisGuard,
    limit: usize,
    job: Job,
) where
    Job: Fn(String) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(limit.max(1)));
    let mut jobs = tokio::task::JoinSet::new();
    for track_id in track_ids {
        if analysis.checkpoint().is_err() {
            break;
        }
        let permits = std::sync::Arc::clone(&permits);
        let job = job(track_id.clone());
        jobs.spawn(async move {
            let Ok(_permit) = permits.acquire().await else {
                return;
            };
            job.await;
        });
    }
    while let Some(result) = jobs.join_next().await {
        if let Err(error) = result {
            log::error!("[preprocessing] track job panicked: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::run_bounded_track_jobs;
    use crate::preprocessing::AnalysisTaskGroup;

    /// An import batch is routinely much larger than the SQLite pool. The bound
    /// is what keeps every job's connection acquire inside the pool's budget —
    /// unbounded, a batch this size fails with `pool timed out`.
    #[tokio::test]
    async fn a_batch_wider_than_the_pool_never_starves_it() {
        const POOL: u32 = 2;
        const TRACKS: usize = 32;

        let pool = SqlitePoolOptions::new()
            .max_connections(POOL)
            .acquire_timeout(Duration::from_millis(250))
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        let group = AnalysisTaskGroup::new();
        let analysis = group.lease(group.current_epoch().unwrap()).unwrap().guard();

        let track_ids: Vec<String> = (0..TRACKS).map(|i| i.to_string()).collect();
        let ran = Arc::new(AtomicUsize::new(0));
        let starved = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        run_bounded_track_jobs(&track_ids, &analysis, POOL as usize, |_track_id| {
            let pool = pool.clone();
            let ran = Arc::clone(&ran);
            let starved = Arc::clone(&starved);
            let in_flight = Arc::clone(&in_flight);
            let peak = Arc::clone(&peak);
            async move {
                let live = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(live, Ordering::SeqCst);
                match pool.acquire().await {
                    Ok(connection) => {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        drop(connection);
                    }
                    Err(_) => {
                        starved.fetch_add(1, Ordering::SeqCst);
                    }
                }
                in_flight.fetch_sub(1, Ordering::SeqCst);
                ran.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await;

        assert_eq!(starved.load(Ordering::SeqCst), 0, "pool acquire timed out");
        assert_eq!(ran.load(Ordering::SeqCst), TRACKS, "every track must run");
        assert!(
            peak.load(Ordering::SeqCst) <= POOL as usize,
            "bound was exceeded: {} in flight",
            peak.load(Ordering::SeqCst)
        );
    }
}

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Json,
    extract::{Query, State},
};
use liquid_cache_common::rpc::ExecutionMetricsResponse;
use log::info;
use serde::Serialize;
use uuid::Uuid;

use super::{ApiResponse, AppState};

pub(crate) async fn shutdown_handler() -> Json<ApiResponse> {
    info!("Shutdown request received, shutting down server...");

    tokio::spawn(async {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        std::process::exit(0);
    });

    Json(ApiResponse {
        message: "Server shutting down...".to_string(),
        status: "success".to_string(),
    })
}

pub(crate) async fn reset_cache_handler(State(state): State<Arc<AppState>>) -> Json<ApiResponse> {
    info!("Resetting cache...");
    if let Some(cache) = state.liquid_cache.cache() {
        cache.reset();
    }

    Json(ApiResponse {
        message: "Cache reset successfully".to_string(),
        status: "success".to_string(),
    })
}

#[derive(Serialize)]
pub(crate) struct ParquetCacheUsage {
    directory: String,
    file_count: usize,
    total_size_bytes: u64,
    status: String,
}

fn get_parquet_cache_usage_inner(cache_dir: &Path) -> ParquetCacheUsage {
    let mut file_count = 0;
    let mut total_size: u64 = 0;

    fn walk_dir(dir: &Path) -> Result<(usize, u64), std::io::Error> {
        let mut count = 0;
        let mut size = 0;

        if dir.exists() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();

                if path.is_file() {
                    count += 1;
                    let metadata = fs::metadata(&path)?;
                    size += metadata.len();
                } else if path.is_dir() {
                    let (sub_count, sub_size) = walk_dir(&path)?;
                    count += sub_count;
                    size += sub_size;
                }
            }
        }

        Ok((count, size))
    }

    if let Ok((count, size)) = walk_dir(cache_dir) {
        file_count = count;
        total_size = size;
    }

    ParquetCacheUsage {
        directory: cache_dir.to_string_lossy().to_string(),
        file_count,
        total_size_bytes: total_size,
        status: "success".to_string(),
    }
}

pub(crate) async fn get_parquet_cache_usage_handler(
    State(state): State<Arc<AppState>>,
) -> Json<ParquetCacheUsage> {
    info!("Getting parquet cache usage...");
    let cache_dir = state.liquid_cache.get_parquet_cache_dir();
    let usage = get_parquet_cache_usage_inner(cache_dir);
    Json(usage)
}

#[derive(Serialize)]
pub(crate) struct CacheInfo {
    batch_size: usize,
    max_cache_bytes: u64,
    memory_usage_bytes: u64,
    disk_usage_bytes: u64,
}

pub(crate) async fn get_cache_info_handler(State(state): State<Arc<AppState>>) -> Json<CacheInfo> {
    info!("Getting cache info...");
    let Some(cache) = state.liquid_cache.cache() else {
        return Json(CacheInfo {
            batch_size: 0,
            max_cache_bytes: 0,
            memory_usage_bytes: 0,
            disk_usage_bytes: 0,
        });
    };
    let batch_size = cache.batch_size();
    let max_cache_bytes = cache.max_cache_bytes() as u64;
    let memory_usage_bytes = cache.memory_usage_bytes() as u64;
    let disk_usage_bytes = cache.disk_usage_bytes() as u64;
    Json(CacheInfo {
        batch_size,
        max_cache_bytes,
        memory_usage_bytes,
        disk_usage_bytes,
    })
}

#[derive(Serialize)]
pub(crate) struct SystemInfo {
    total_memory_bytes: u64,
    used_memory_bytes: u64,
    available_memory_bytes: u64,
    name: String,
    kernel: String,
    os: String,
    host_name: String,
    cpu_cores: usize,
    server_resident_memory_bytes: u64,
    server_virtual_memory_bytes: u64,
}

pub(crate) async fn get_system_info_handler(
    State(_state): State<Arc<AppState>>,
) -> Json<SystemInfo> {
    info!("Getting system info...");
    let mut sys = sysinfo::System::new_all();
    sys.refresh_all();
    let current_pid = sysinfo::get_current_pid().unwrap();
    let process = sys.process(current_pid).unwrap();
    let resident_memory = process.memory();
    let virtual_memory = process.virtual_memory();
    Json(SystemInfo {
        total_memory_bytes: sys.total_memory(),
        used_memory_bytes: sys.used_memory(),
        available_memory_bytes: sys.available_memory(),
        name: sysinfo::System::name().unwrap_or_default(),
        kernel: sysinfo::System::kernel_version().unwrap_or_default(),
        os: sysinfo::System::os_version().unwrap_or_default(),
        host_name: sysinfo::System::host_name().unwrap_or_default(),
        cpu_cores: sysinfo::System::physical_core_count().unwrap_or(0),
        server_resident_memory_bytes: resident_memory,
        server_virtual_memory_bytes: virtual_memory,
    })
}

#[derive(serde::Deserialize)]
pub(crate) struct TraceParams {
    path: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct ExecutionMetricsParams {
    plan_id: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct CacheStatsParams {
    path: String,
}

pub(crate) async fn start_trace_handler(State(state): State<Arc<AppState>>) -> Json<ApiResponse> {
    info!("Starting cache trace collection...");
    if let Some(cache) = state.liquid_cache.cache() {
        cache.enable_trace();
    }

    Json(ApiResponse {
        message: "Cache trace collection started".to_string(),
        status: "success".to_string(),
    })
}

pub(crate) async fn stop_trace_handler(
    Query(params): Query<TraceParams>,
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse> {
    info!("Stopping cache trace collection...");
    let save_path = Path::new(&params.path);

    match save_trace_to_file(save_path, &state) {
        Ok(_) => Json(ApiResponse {
            message: format!(
                "Cache trace collection stopped, saved to {}",
                save_path.display()
            ),
            status: "success".to_string(),
        }),
        Err(e) => Json(ApiResponse {
            message: format!("Failed to save trace: {e}"),
            status: "error".to_string(),
        }),
    }
}

pub(crate) fn save_trace_to_file(
    save_dir: &Path,
    state: &AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = std::time::SystemTime::now();
    let datetime = now.duration_since(std::time::UNIX_EPOCH).unwrap();
    let minute = (datetime.as_secs() / 60) % 60;
    let second = datetime.as_secs() % 60;
    let trace_id = state
        .trace_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let filename = format!("cache-trace-id{trace_id:02}-{minute:02}-{second:03}.parquet",);

    // Ensure directory exists
    if !save_dir.exists() {
        fs::create_dir_all(save_dir)?;
    }

    let file_path = save_dir.join(filename);
    if let Some(cache) = state.liquid_cache.cache() {
        cache.disable_trace();
        cache.flush_trace(&file_path);
    }
    Ok(())
}

pub(crate) async fn get_execution_metrics_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ExecutionMetricsParams>,
) -> Json<Option<ExecutionMetricsResponse>> {
    let Ok(uuid) = Uuid::parse_str(&params.plan_id) else {
        return Json(None);
    };
    let metrics = state.liquid_cache.inner().get_metrics(&uuid);
    Json(metrics)
}

pub(crate) fn get_cache_stats_inner(
    cache: &liquid_cache_parquet::LiquidCacheRef,
    save_dir: impl AsRef<Path>,
    state: &AppState,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let now = std::time::SystemTime::now();
    let datetime = now.duration_since(std::time::UNIX_EPOCH).unwrap();
    let minute = (datetime.as_secs() / 60) % 60;
    let second = datetime.as_secs() % 60;
    let trace_id = state
        .stats_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let filename = format!("cache-stats-id{trace_id:02}-{minute:02}-{second:03}.parquet",);
    let file_path = save_dir.as_ref().join(filename);
    cache.write_stats(&file_path)?;
    Ok(file_path)
}

pub(crate) async fn get_cache_stats_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CacheStatsParams>,
) -> Json<ApiResponse> {
    let Some(cache) = state.liquid_cache.cache() else {
        return Json(ApiResponse {
            message: "Cache not enabled".to_string(),
            status: "error".to_string(),
        });
    };
    match get_cache_stats_inner(cache, &params.path, &state) {
        Ok(file_path) => {
            info!("Cache stats saved to {}", file_path.display());
            Json(ApiResponse {
                message: format!("Cache stats saved to {}", file_path.display()),
                status: "success".to_string(),
            })
        }
        Err(e) => Json(ApiResponse {
            message: format!("Failed to get cache stats: {e}"),
            status: "error".to_string(),
        }),
    }
}

pub(crate) async fn start_flamegraph_handler(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse> {
    info!("Starting flamegraph collection...");
    state.flamegraph.start();
    Json(ApiResponse {
        message: "Flamegraph collection started".to_string(),
        status: "success".to_string(),
    })
}

#[derive(serde::Deserialize)]
pub(crate) struct FlameGraphParams {
    output_dir: String,
}

pub(crate) async fn stop_flamegraph_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<FlameGraphParams>,
) -> Json<ApiResponse> {
    let output_dir = PathBuf::from(&params.output_dir);
    let filepath = state.flamegraph.stop(&output_dir);
    info!(
        "Flamegraph collection stopped, saved to {}",
        filepath.display()
    );
    Json(ApiResponse {
        message: format!(
            "Flamegraph collection stopped, saved to {}",
            filepath.display()
        ),
        status: "success".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::{io::Write, path::PathBuf};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_get_parquet_cache_usage_inner() {
        let temp_dir = tempdir().unwrap();
        let temp_path = temp_dir.path();

        let file1_path = temp_path.join("file1.parquet");
        let file2_path = temp_path.join("file2.parquet");

        let subdir_path = temp_path.join("subdir");
        std::fs::create_dir(&subdir_path).unwrap();
        let file3_path = subdir_path.join("file3.parquet");

        let data1 = [1u8; 1000];
        let data2 = [2u8; 2000];
        let data3 = [3u8; 3000];

        let mut file1 = std::fs::File::create(&file1_path).unwrap();
        file1.write_all(&data1).unwrap();

        let mut file2 = std::fs::File::create(&file2_path).unwrap();
        file2.write_all(&data2).unwrap();

        let mut file3 = std::fs::File::create(&file3_path).unwrap();
        file3.write_all(&data3).unwrap();

        // Expected total size: 6000 bytes (1000 + 2000 + 3000)

        let result = get_parquet_cache_usage_inner(temp_path);
        assert_eq!(result.directory, temp_path.to_string_lossy().to_string());
        assert_eq!(result.file_count, 3);
        assert_eq!(result.total_size_bytes, 6000);
        assert_eq!(result.status, "success");
    }

    #[test]
    fn test_get_parquet_cache_usage_inner_empty_dir() {
        let temp_dir = tempdir().unwrap();
        let temp_path = temp_dir.path();
        let result = get_parquet_cache_usage_inner(temp_path);
        assert_eq!(result.file_count, 0);
        assert_eq!(result.total_size_bytes, 0);
    }

    #[test]
    fn test_get_parquet_cache_usage_inner_nonexistent_dir() {
        let nonexistent_path = PathBuf::from("/path/does/not/exist");
        let result = get_parquet_cache_usage_inner(&nonexistent_path);
        assert_eq!(result.file_count, 0);
        assert_eq!(result.total_size_bytes, 0);
    }
}

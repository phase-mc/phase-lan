//! Session report: ping by path, path switches, traffic, losses, voice counters.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use phase_lang::tr;
use phase_tunnel::{endpoint, metrics};

/// Writes the report to `<dir>/phase-<role>-<time>.metrics.json`.
/// Returns the path to the file, or `None` if there is nothing to write.
pub fn save(
    dir: &Path,
    role: &str,
    telemetry: &metrics::Telemetry,
    extra: Option<serde_json::Value>,
) -> Result<Option<PathBuf>> {
    let connections = telemetry.reports();
    if connections.is_empty() && extra.is_none() {
        return Ok(None);
    }
    let doc = serde_json::json!({
        "role": role,
        "app_version": env!("CARGO_PKG_VERSION"),
        "saved_unix_s": metrics::unix_now(),
        "transport": {
            "idle_timeout_s": endpoint::IDLE_TIMEOUT.as_secs_f64(),
            "keep_alive_s": endpoint::KEEP_ALIVE.as_secs_f64(),
            "path_keep_alive_s": endpoint::PATH_KEEP_ALIVE.as_secs_f64(),
            "path_idle_timeout_s": endpoint::PATH_IDLE_TIMEOUT.as_secs_f64(),
        },
        "connections": connections,
        "echo": extra,
    });
    std::fs::create_dir_all(dir).with_context(|| {
        tr!(
            format!("не удалось создать папку {}", dir.display()),
            format!("could not create the folder {}", dir.display())
        )
    })?;
    let file = dir.join(format!("phase-{role}-{}.metrics.json", metrics::unix_now()));
    std::fs::write(&file, serde_json::to_vec_pretty(&doc)?).with_context(|| {
        tr!(
            format!("не удалось записать {}", file.display()),
            format!("could not write {}", file.display())
        )
    })?;
    Ok(Some(file))
}

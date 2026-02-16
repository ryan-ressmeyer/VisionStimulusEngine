//! Host information capture logic
//!
//! Collects system information from sysinfo, environment variables,
//! and other OS-level sources.

use sysinfo::System;

use super::host_info::{CpuInfo, MemoryInfo, OsInfo, RuntimeEnv};

/// Capture operating system information
pub fn capture_os_info() -> OsInfo {
    OsInfo {
        name: System::name().unwrap_or_else(|| "unknown".to_string()),
        version: System::os_version().unwrap_or_else(|| "unknown".to_string()),
        kernel_version: System::kernel_version().unwrap_or_else(|| "unknown".to_string()),
        hostname: System::host_name().unwrap_or_else(|| "unknown".to_string()),
    }
}

/// Capture CPU information
pub fn capture_cpu_info() -> CpuInfo {
    let sys = System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_cpu(sysinfo::CpuRefreshKind::everything()),
    );
    let cpus = sys.cpus();
    let brand = cpus
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let frequency = cpus.first().map(|c| c.frequency()).unwrap_or(0);

    CpuInfo {
        brand,
        physical_cores: sys.physical_core_count().unwrap_or(0),
        logical_cores: cpus.len(),
        frequency_mhz: frequency,
    }
}

/// Capture memory information
pub fn capture_memory_info() -> MemoryInfo {
    let sys = System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_memory(sysinfo::MemoryRefreshKind::everything()),
    );

    MemoryInfo {
        total_bytes: sys.total_memory(),
        available_bytes: sys.available_memory(),
        used_bytes: sys.used_memory(),
    }
}

/// Capture runtime environment information
pub fn capture_runtime_env() -> RuntimeEnv {
    let display_server = if std::env::var("WAYLAND_DISPLAY").is_ok() {
        "wayland".to_string()
    } else if std::env::var("DISPLAY").is_ok() {
        "x11".to_string()
    } else {
        "unknown".to_string()
    };

    let nice_value = get_process_nice_value();

    RuntimeEnv {
        display_server,
        env_display: std::env::var("DISPLAY").ok(),
        env_wayland_display: std::env::var("WAYLAND_DISPLAY").ok(),
        env_vk_icd_filenames: std::env::var("VK_ICD_FILENAMES").ok(),
        env_vk_layer_path: std::env::var("VK_LAYER_PATH").ok(),
        process_nice_value: nice_value,
    }
}

/// Get the process nice value on Linux
fn get_process_nice_value() -> Option<i32> {
    #[cfg(target_os = "linux")]
    {
        // Read from /proc/self/stat — field 19 is the nice value
        std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|stat| {
                // Fields are space-separated, but field 2 (comm) may contain spaces
                // and is wrapped in parens. Find the closing paren first.
                let after_comm = stat.find(')')?.checked_add(2)?;
                let fields: Vec<&str> = stat[after_comm..].split_whitespace().collect();
                // After comm: state(0), ppid(1), pgrp(2), session(3), tty(4),
                // tpgid(5), flags(6), minflt(7), cminflt(8), majflt(9),
                // cmajflt(10), utime(11), stime(12), cutime(13), cstime(14),
                // priority(15), nice(16)
                fields.get(16)?.parse::<i32>().ok()
            })
    }

    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_os_info() {
        let os = capture_os_info();
        assert!(!os.name.is_empty() || os.name == "unknown");
        assert!(!os.hostname.is_empty() || os.hostname == "unknown");
    }

    #[test]
    fn test_capture_cpu_info() {
        let cpu = capture_cpu_info();
        assert!(cpu.logical_cores > 0);
        assert!(!cpu.brand.is_empty() || cpu.brand == "unknown");
    }

    #[test]
    fn test_capture_memory_info() {
        let mem = capture_memory_info();
        assert!(mem.total_bytes > 0);
        assert!(mem.used_bytes <= mem.total_bytes);
    }

    #[test]
    fn test_capture_runtime_env() {
        let env = capture_runtime_env();
        assert!(
            env.display_server == "x11"
                || env.display_server == "wayland"
                || env.display_server == "unknown"
        );
    }
}

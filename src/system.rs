//! CPU and RAM readings through `sysinfo`.

use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

/// Bytes in one gibibyte, the unit Task Manager calls "GB".
const BYTES_PER_GB: f64 = 1024.0 * 1024.0 * 1024.0;

/// One reading of system load.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Load {
    /// Average CPU usage across all cores, from 0 to 1.
    pub cpu: f32,
    pub ram_used_bytes: u64,
    pub ram_total_bytes: u64,
}

impl Load {
    /// RAM in use, from 0 to 1.
    #[must_use]
    pub fn ram_fraction(&self) -> f32 {
        fraction(self.ram_used_bytes, self.ram_total_bytes)
    }
}

/// Source of [`Load`] readings. CPU usage is measured between two calls to
/// [`SystemLoad::sample`], so the first reading reports 0% CPU.
#[derive(Debug)]
pub struct SystemLoad {
    system: System,
}

impl SystemLoad {
    #[must_use]
    pub fn new() -> Self {
        let refresh = RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram());
        Self {
            system: System::new_with_specifics(refresh),
        }
    }

    pub fn sample(&mut self) -> Load {
        self.system.refresh_cpu_usage();
        self.system
            .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        Load {
            cpu: self.system.global_cpu_usage() / 100.0,
            ram_used_bytes: self.system.used_memory(),
            ram_total_bytes: self.system.total_memory(),
        }
    }
}

impl Default for SystemLoad {
    fn default() -> Self {
        Self::new()
    }
}

/// `used / total` clamped to 0..=1, or 0 when the total is unknown.
#[must_use]
pub fn fraction(used: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    // Precision loss on huge byte counts does not matter for a progress bar.
    (used as f64 / total as f64).clamp(0.0, 1.0) as f32
}

/// Formats a byte count like Task Manager does, e.g. "9.8 GB".
#[must_use]
pub fn gigabytes(bytes: u64) -> String {
    // Display rounding only; precision loss is irrelevant here.
    format!("{:.1} GB", bytes as f64 / BYTES_PER_GB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gigabytes_uses_binary_units_and_one_decimal() {
        assert_eq!(gigabytes(0), "0.0 GB");
        assert_eq!(gigabytes(10_522_669_875), "9.8 GB");
        assert_eq!(gigabytes(16 * 1024 * 1024 * 1024), "16.0 GB");
    }

    #[test]
    fn fraction_handles_unknown_total_and_overflow() {
        assert_eq!(fraction(5, 0), 0.0);
        assert_eq!(fraction(3, 4), 0.75);
        assert_eq!(fraction(9, 4), 1.0);
    }

    #[test]
    fn sample_reports_values_in_range() {
        let mut load = SystemLoad::new();
        let first = load.sample();
        assert!((0.0..=1.0).contains(&first.cpu), "cpu {}", first.cpu);
        assert!(
            first.ram_total_bytes > 0,
            "ram total {}",
            first.ram_total_bytes
        );
        assert!((0.0..=1.0).contains(&first.ram_fraction()), "{first:?}");
    }
}

//! GPU utilization and VRAM through Windows performance counters (any vendor).
//!
//! Utilization follows Task Manager: sum each engine's usage across processes, then
//! report the busiest engine. VRAM is reported for the adapter with the most dedicated
//! memory, found through DXGI.

use std::collections::HashMap;
use std::mem;

use anyhow::{Context, bail};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};
use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::core::{PCWSTR, w};

/// One reading of GPU load.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GpuReading {
    /// Busiest engine's usage, from 0 to 1.
    pub utilization: f32,
    /// Dedicated memory in use on the main adapter, when it could be matched.
    pub vram_used_bytes: Option<u64>,
    /// Dedicated memory size of the main adapter.
    pub vram_total_bytes: Option<u64>,
}

/// The adapter whose VRAM is shown: the one with the most dedicated memory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MainAdapter {
    /// PDH instance prefix for this adapter, e.g. `luid_0x00000000_0x00017386`.
    luid: String,
    dedicated_bytes: u64,
}

/// Source of [`GpuReading`]s. Usage is a rate, so the first reading reports 0%.
#[derive(Debug)]
pub struct GpuLoad {
    query: Query,
    engines: PDH_HCOUNTER,
    memory: PDH_HCOUNTER,
    main_adapter: Option<MainAdapter>,
}

impl GpuLoad {
    /// Opens the performance counters and finds the main adapter.
    ///
    /// # Errors
    ///
    /// Fails when the GPU performance counters are unavailable, e.g. with no WDDM
    /// 2.0+ driver installed.
    pub fn open() -> anyhow::Result<Self> {
        let query = Query::open()?;
        let engines = query.add_counter(w!(r"\GPU Engine(*)\Utilization Percentage"))?;
        let memory = query.add_counter(w!(r"\GPU Adapter Memory(*)\Dedicated Usage"))?;
        // A missing adapter only hides the VRAM total; utilization still works.
        let main_adapter = main_adapter().ok().flatten();
        Ok(Self {
            query,
            engines,
            memory,
            main_adapter,
        })
    }

    /// Takes a reading. Values that Windows cannot provide right now read as 0 or `None`.
    pub fn sample(&mut self) -> GpuReading {
        if self.query.collect().is_err() {
            return GpuReading::default();
        }
        let utilization = read_array(self.engines)
            .map(|items| busiest_engine(&items) / 100.0)
            .unwrap_or(0.0);
        let vram_used_bytes = self.main_adapter.as_ref().and_then(|adapter| {
            let items = read_array(self.memory).ok()?;
            items
                .iter()
                .find(|(name, _)| name.starts_with(&adapter.luid))
                // Byte counts arrive as f64; whole bytes are all we need.
                .map(|(_, bytes)| *bytes as u64)
        });
        GpuReading {
            // Usage percentages fit comfortably in f32.
            utilization: (utilization as f32).clamp(0.0, 1.0),
            vram_used_bytes,
            vram_total_bytes: self.main_adapter.as_ref().map(|a| a.dedicated_bytes),
        }
    }
}

/// An open PDH query, closed on drop.
#[derive(Debug)]
struct Query(PDH_HQUERY);

impl Query {
    fn open() -> anyhow::Result<Self> {
        let mut handle = PDH_HQUERY::default();
        // SAFETY: `handle` is a valid out-pointer; a null data source means live data.
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut handle) };
        check(status, "PdhOpenQueryW")?;
        Ok(Self(handle))
    }

    fn add_counter(&self, path: PCWSTR) -> anyhow::Result<PDH_HCOUNTER> {
        let mut counter = PDH_HCOUNTER::default();
        // SAFETY: the query is open, `path` is a static wide string, `counter` is valid.
        let status = unsafe { PdhAddEnglishCounterW(self.0, path, 0, &mut counter) };
        // SAFETY: `path` came from `w!`, so it is a valid NUL-terminated wide string.
        let name = unsafe { path.to_string() }.unwrap_or_default();
        check(status, &format!("PdhAddEnglishCounterW({name})"))?;
        Ok(counter)
    }

    fn collect(&self) -> anyhow::Result<()> {
        // SAFETY: the query is open.
        check(
            unsafe { PdhCollectQueryData(self.0) },
            "PdhCollectQueryData",
        )
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: the query is open and closed exactly once, here.
        unsafe { PdhCloseQuery(self.0) };
    }
}

fn check(status: u32, call: &str) -> anyhow::Result<()> {
    if status != 0 {
        bail!("{call} failed with PDH status {status:#010x}");
    }
    Ok(())
}

/// Reads every instance of a wildcard counter as `(instance name, value)` pairs.
fn read_array(counter: PDH_HCOUNTER) -> anyhow::Result<Vec<(String, f64)>> {
    // Instances can appear between the size query and the read, so retry a few times.
    for _ in 0..3 {
        let (mut bytes, mut count) = (0u32, 0u32);
        // SAFETY: a null buffer asks PDH for the required size only.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, None)
        };
        if status != PDH_MORE_DATA {
            check(status, "PdhGetFormattedCounterArrayW (size)")?;
            return Ok(Vec::new());
        }

        // The buffer holds the items followed by their name strings, so size it in items
        // rounded up to keep the alignment PDH expects.
        let item_size = mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let slots = (bytes as usize).div_ceil(item_size);
        let mut buffer = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); slots];
        // SAFETY: `buffer` is at least `bytes` long and suitably aligned for the items.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                Some(buffer.as_mut_ptr()),
            )
        };
        if status == PDH_MORE_DATA {
            continue;
        }
        check(status, "PdhGetFormattedCounterArrayW")?;

        let items = &buffer[..count as usize];
        let values = items
            .iter()
            .filter(|item| {
                matches!(
                    item.FmtValue.CStatus,
                    PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA
                )
            })
            .filter_map(|item| {
                // SAFETY: names point into `buffer`, which is alive, and are NUL-terminated.
                let name = unsafe { item.szName.to_string() }.ok()?;
                // SAFETY: PDH_FMT_DOUBLE was requested, so the union holds a double.
                let value = unsafe { item.FmtValue.Anonymous.doubleValue };
                Some((name, value))
            })
            .collect();
        return Ok(values);
    }
    bail!("GPU counter instances kept changing while being read")
}

/// The engine part of a `GPU Engine` instance name, without the process id, e.g.
/// `luid_0x00000000_0x000137c4_phys_0_eng_0` from
/// `pid_13560_luid_0x00000000_0x000137c4_phys_0_eng_0_engtype_3d`.
fn engine_key(instance: &str) -> Option<&str> {
    let start = instance.find("luid_")?;
    let end = instance.find("_engtype_")?;
    instance.get(start..end)
}

/// The busiest engine's usage in percent, summing each engine across processes.
fn busiest_engine(samples: &[(String, f64)]) -> f64 {
    let mut per_engine: HashMap<&str, f64> = HashMap::new();
    for (instance, value) in samples {
        if let Some(key) = engine_key(instance) {
            *per_engine.entry(key).or_default() += value;
        }
    }
    per_engine.into_values().fold(0.0, f64::max).min(100.0)
}

/// The PDH instance prefix for an adapter LUID; PDH prints the high part first.
fn luid_prefix(high: i32, low: u32) -> String {
    // The high part is printed as its raw 32 bits.
    format!("luid_0x{:08x}_0x{low:08x}", high as u32)
}

fn main_adapter() -> anyhow::Result<Option<MainAdapter>> {
    // SAFETY: plain COM factory creation with no preconditions.
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.context("CreateDXGIFactory1 failed")?;
    let mut best: Option<MainAdapter> = None;
    for index in 0.. {
        // SAFETY: `factory` is a valid COM object. Past the last adapter this returns
        // DXGI_ERROR_NOT_FOUND, which ends the loop.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        // SAFETY: `adapter` is a valid COM object.
        let desc = unsafe { adapter.GetDesc1() }.context("IDXGIAdapter1::GetDesc1 failed")?;
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        // usize to u64 never truncates on 64-bit Windows.
        let dedicated_bytes = desc.DedicatedVideoMemory as u64;
        if best
            .as_ref()
            .is_none_or(|b| dedicated_bytes > b.dedicated_bytes)
        {
            best = Some(MainAdapter {
                luid: luid_prefix(desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart),
                dedicated_bytes,
            });
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, value: f64) -> (String, f64) {
        (name.to_owned(), value)
    }

    #[test]
    fn engine_key_drops_pid_and_engine_type() {
        assert_eq!(
            engine_key("pid_13560_luid_0x00000000_0x000137c4_phys_0_eng_0_engtype_3d"),
            Some("luid_0x00000000_0x000137c4_phys_0_eng_0")
        );
        assert_eq!(
            engine_key("pid_1_luid_0x00000000_0x000137c4_phys_0_eng_12_engtype_"),
            Some("luid_0x00000000_0x000137c4_phys_0_eng_12")
        );
        assert_eq!(engine_key("_Total"), None);
    }

    #[test]
    fn busiest_engine_sums_processes_then_takes_max() {
        let samples = [
            sample("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3d", 20.0),
            sample("pid_2_luid_0x0_0x1_phys_0_eng_0_engtype_3d", 25.0),
            sample("pid_1_luid_0x0_0x1_phys_0_eng_3_engtype_copy", 30.0),
            sample("pid_3_luid_0x0_0x2_phys_0_eng_0_engtype_3d", 10.0),
        ];
        assert_eq!(busiest_engine(&samples), 45.0);
    }

    #[test]
    fn busiest_engine_caps_at_100_and_handles_no_samples() {
        let samples = [
            sample("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3d", 70.0),
            sample("pid_2_luid_0x0_0x1_phys_0_eng_0_engtype_3d", 50.0),
        ];
        assert_eq!(busiest_engine(&samples), 100.0);
        assert_eq!(busiest_engine(&[]), 0.0);
    }

    #[test]
    fn live_counters_report_values_in_range() {
        let mut gpu = GpuLoad::open().expect("GPU counters exist on the development machine");
        gpu.sample();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let reading = gpu.sample();
        assert!((0.0..=1.0).contains(&reading.utilization), "{reading:?}");
        if let (Some(used), Some(total)) = (reading.vram_used_bytes, reading.vram_total_bytes) {
            assert!(total > 0 && used <= total, "{reading:?}");
        }
    }

    #[test]
    fn luid_prefix_matches_pdh_instance_names() {
        assert_eq!(luid_prefix(0, 0x0001_7386), "luid_0x00000000_0x00017386");
        assert_eq!(luid_prefix(-1, 1), "luid_0xffffffff_0x00000001");
    }
}

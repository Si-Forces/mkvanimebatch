/// Counts physical cores in `/proc/cpuinfo` content as unique (physical id, core id) pairs.
/// Returns 0 when no `core id` fields are present (ARM, VMs without exposed topology).
#[cfg(any(target_os = "linux", test))]
fn parse_cpuinfo_topology(content: &str) -> usize {
    let mut cores = std::collections::HashSet::new();
    let mut current_physical_id = "";
    let mut current_core_id = None;

    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("processor") {
            if let Some(c) = current_core_id {
                cores.insert((current_physical_id, c));
            }
            current_physical_id = "";
            current_core_id = None;
        }
        if let Some(rest) = trimmed.strip_prefix("physical id") {
            if let Some((_, id)) = rest.split_once(':') {
                current_physical_id = id.trim();
            }
        } else if let Some(rest) = trimmed.strip_prefix("core id") {
            if let Some((_, id)) = rest.split_once(':') {
                current_core_id = Some(id.trim());
            }
        }
    }
    if let Some(c) = current_core_id {
        cores.insert((current_physical_id, c));
    }
    cores.len()
}

/// Returns the number of physical CPU cores, falling back to hardware concurrency.
///
/// On Linux, parses `/proc/cpuinfo` topology (tracking physical ID and core ID pairs).
/// On Windows, macOS, or virtualized/container environments without exposed topology,
/// falls back to `std::thread::available_parallelism()`.
pub(crate) fn physical_core_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
            let count = parse_cpuinfo_topology(&cpuinfo);
            if count > 0 {
                return count;
            }
        }
    }
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
}

/// Derives the per-job FFmpeg thread count from physical cores and active batch workers
/// per ADR-0013.
pub(crate) fn transcode_governor_threads(cores: usize, workers: usize) -> usize {
    (cores / workers.max(1)).clamp(1, 4)
}

/// Default batch worker count derived from physical CPU cores, clamped to [1, 16].
pub(crate) fn default_workers() -> usize {
    physical_core_count().clamp(1, 16)
}

/// Derives preflight or execution probe semaphore concurrency from active/default workers.
pub(crate) fn probe_concurrency(workers: usize) -> usize {
    (workers * 2).clamp(2, 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transcode_governor_threads() {
        // Boundary cases: workers or cores 0
        assert_eq!(transcode_governor_threads(0, 0), 1);
        assert_eq!(transcode_governor_threads(4, 0), 4);
        assert_eq!(transcode_governor_threads(0, 4), 1);

        // cores < workers (clamped to 1)
        assert_eq!(transcode_governor_threads(2, 8), 1);

        // cores == workers
        assert_eq!(transcode_governor_threads(4, 4), 1);

        // intermediate ratios
        assert_eq!(transcode_governor_threads(6, 2), 3);
        assert_eq!(transcode_governor_threads(8, 2), 4);

        // high core to worker ratios (clamped to 4)
        assert_eq!(transcode_governor_threads(32, 2), 4);
        assert_eq!(transcode_governor_threads(64, 4), 4);
    }

    #[test]
    fn test_probe_concurrency_clamps() {
        assert_eq!(probe_concurrency(0), 2);
        assert_eq!(probe_concurrency(1), 2);
        assert_eq!(probe_concurrency(4), 8);
        assert_eq!(probe_concurrency(8), 16);
        assert_eq!(probe_concurrency(32), 16);
    }

    #[test]
    fn test_parse_cpuinfo_topology() {
        // 1. Synthetic standard x86 dual-core hyperthreaded (4 logical processors, 2 physical cores)
        let sample_x86 = "processor\t: 0\nphysical id\t: 0\ncore id\t\t: 0\n\n\
                          processor\t: 1\nphysical id\t: 0\ncore id\t\t: 0\n\n\
                          processor\t: 2\nphysical id\t: 0\ncore id\t\t: 1\n\n\
                          processor\t: 3\nphysical id\t: 0\ncore id\t\t: 1\n";
        assert_eq!(parse_cpuinfo_topology(sample_x86), 2);

        // 2. Multi-socket with spaces instead of tabs and core id before physical id
        let sample_multisocket = "processor: 0\n  core id: 0\n  physical id: 0\n\n\
                                  processor: 1\n  core id: 1\n  physical id: 0\n\n\
                                  processor: 2\n  core id: 0\n  physical id: 1\n\n\
                                  processor: 3\n  core id: 1\n  physical id: 1\n";
        assert_eq!(parse_cpuinfo_topology(sample_multisocket), 4);

        // 3. ARM / VM container cpuinfo without core id or physical id
        let sample_arm = "processor\t: 0\nBogoMIPS\t: 48.00\nFeatures\t: fp asimd\n\n\
                          processor\t: 1\nBogoMIPS\t: 48.00\nFeatures\t: fp asimd\n";
        assert_eq!(parse_cpuinfo_topology(sample_arm), 0);

        // 4. Empty string
        assert_eq!(parse_cpuinfo_topology(""), 0);

        // 5. Stanzas separated only by processor line without blank lines
        let sample_no_blank_lines = "processor: 0\nphysical id: 0\ncore id: 0\n\
                                     processor: 1\nphysical id: 0\ncore id: 1\n";
        assert_eq!(parse_cpuinfo_topology(sample_no_blank_lines), 2);

        // 6. Stanzas with core id but no physical id — counted under the anonymous socket ("").
        //    Two distinct core ids → 2 unique ("", core_id) pairs.
        let sample_no_phys = "processor: 0\ncore id: 0\n\nprocessor: 1\ncore id: 1\n";
        assert_eq!(parse_cpuinfo_topology(sample_no_phys), 2);
    }
}

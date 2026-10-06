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

/// Caps the topology core count at the parallelism the process may use.
/// A `topology` of 0 means no topology was found, so `available` is used as is.
fn effective_core_count(topology: usize, available: usize) -> usize {
    if topology > 0 {
        topology.min(available)
    } else {
        available
    }
}

/// Counts the `/proc/cpuinfo` topology, or 0 when the file can't be read or exposes none.
#[cfg(target_os = "linux")]
fn topology_core_count() -> usize {
    std::fs::read_to_string("/proc/cpuinfo")
        .map(|cpuinfo| parse_cpuinfo_topology(&cpuinfo))
        .unwrap_or(0)
}

/// Off Linux there is no topology source.
#[cfg(not(target_os = "linux"))]
fn topology_core_count() -> usize {
    0
}

/// Returns the number of physical CPU cores the process may use.
///
/// On Linux, counts `/proc/cpuinfo` topology (physical ID and core ID pairs) and caps it at
/// `std::thread::available_parallelism()`, which accounts for affinity masks and cgroup quotas
/// where it can read them.
/// On Windows, macOS, or virtualized/container environments without exposed topology,
/// returns `available_parallelism()` alone.
pub(crate) fn physical_core_count() -> usize {
    let available = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4);
    effective_core_count(topology_core_count(), available)
}

/// Derives the per-job FFmpeg `-threads` value from physical cores and active batch workers
/// per ADR-0013.
///
/// `-threads` precedes `-i`, so it caps the FLAC decoder's threads, not the encoder's: libopus is
/// single-threaded. The cap keeps ffmpeg from sizing decoder threads to all cores in each of
/// `workers` processes (ADR-0013, 2026-10-05 correction, #36).
pub(crate) fn transcode_governor_threads(cores: usize, workers: usize) -> usize {
    (cores / workers.max(1)).clamp(1, 4)
}

/// The default batch worker count for `cores` physical CPU cores, clamped to [1, 16].
///
/// The one home of the rule: `BatchRequest::resolve` applies it to its injected core count.
pub(crate) fn default_workers_for(cores: usize) -> usize {
    cores.clamp(1, 16)
}

/// Default batch worker count derived from physical CPU cores, clamped to [1, 16].
///
/// `BatchRequest::resolve` applies the same rule (`default_workers_for`) to its core count when
/// `workers` is `None`.
pub(crate) fn default_workers() -> usize {
    default_workers_for(physical_core_count())
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
    fn test_default_workers_for_clamps() {
        assert_eq!(default_workers_for(0), 1);
        assert_eq!(default_workers_for(1), 1);
        assert_eq!(default_workers_for(4), 4);
        assert_eq!(default_workers_for(16), 16);
        assert_eq!(default_workers_for(32), 16);
    }

    #[test]
    fn test_effective_core_count_caps_topology_at_available() {
        // Container quota or affinity below the host topology.
        assert_eq!(effective_core_count(16, 2), 2);
        // Unconstrained host: physical cores, not hyperthreads.
        assert_eq!(effective_core_count(8, 16), 8);
        // No topology exposed.
        assert_eq!(effective_core_count(0, 4), 4);
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

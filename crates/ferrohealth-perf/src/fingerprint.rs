// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The host a run was measured on.
//!
//! A latency is comparable only on a like machine, so every report and
//! baseline records the architecture, the operating system, the parallelism
//! the process may use (`std::thread::available_parallelism`, which honours
//! a container's CPU quota), and the CPU model and total memory from
//! `/proc/cpuinfo` and `/proc/meminfo` where the host has them. A field the
//! host does not disclose is absent, never guessed. The fields follow
//! Veredictum's `perf_emulate::fingerprint` (Apache-2.0, credited in
//! `NOTICE`).

use serde::{Deserialize, Serialize};

/// What a run could establish about its host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    /// The target architecture (`std::env::consts::ARCH`).
    pub arch: String,
    /// The target operating system (`std::env::consts::OS`).
    pub os: String,
    /// The parallelism the process may use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_parallelism: Option<u32>,
    /// The CPU model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_model: Option<String>,
    /// Total physical memory, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_memory_bytes: Option<u64>,
}

impl Fingerprint {
    /// Reads the fingerprint of the host this process runs on.
    #[must_use]
    pub fn detect() -> Self {
        let read = |path| std::fs::read_to_string(path).ok();
        Self {
            arch: std::env::consts::ARCH.to_owned(),
            os: std::env::consts::OS.to_owned(),
            available_parallelism: std::thread::available_parallelism()
                .ok()
                .and_then(|count| u32::try_from(count.get()).ok()),
            cpu_model: read("/proc/cpuinfo").as_deref().and_then(cpu_model),
            total_memory_bytes: read("/proc/meminfo")
                .as_deref()
                .and_then(total_memory_bytes),
        }
    }

    /// The fields in which `other` differs from this one, by name.
    #[must_use]
    pub fn differences(&self, other: &Self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.arch != other.arch {
            names.push("arch");
        }
        if self.os != other.os {
            names.push("os");
        }
        if self.available_parallelism != other.available_parallelism {
            names.push("available_parallelism");
        }
        if self.cpu_model != other.cpu_model {
            names.push("cpu_model");
        }
        let gib = |bytes| crate::baseline::Shape::new(None, bytes).memory_bytes;
        if gib(self.total_memory_bytes) != gib(other.total_memory_bytes) {
            names.push("total_memory_bytes");
        }
        names
    }
}

/// The `model name` of a `/proc/cpuinfo` body.
fn cpu_model(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.trim() == "model name")
        })
        .map(|(_, value)| value.trim().to_owned())
        .filter(|model| !model.is_empty())
}

/// The `MemTotal` of a `/proc/meminfo` body, in bytes.
fn total_memory_bytes(text: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_model_is_the_first_model_name() {
        let text = "processor\t: 0\nmodel name\t: AMD EPYC 7763 64-Core Processor\n\
                    processor\t: 1\nmodel name\t: other\n";
        assert_eq!(
            Some("AMD EPYC 7763 64-Core Processor".to_owned()),
            cpu_model(text)
        );
        assert_eq!(None, cpu_model("processor\t: 0\nmodel name\t:\n"));
    }

    #[test]
    fn the_total_memory_is_read_in_kibibytes() {
        assert_eq!(
            Some(16_384 * 1024),
            total_memory_bytes("MemTotal:       16384 kB\nMemFree: 1 kB\n")
        );
        assert_eq!(None, total_memory_bytes("MemFree: 1 kB\n"));
    }

    #[test]
    fn the_differences_are_named() {
        let host = Fingerprint::detect();
        assert!(host.differences(&host).is_empty(), "a host matches itself");
        let mut other = host.clone();
        other.available_parallelism = Some(other.available_parallelism.unwrap_or(0) + 1);
        other.os = "plan9".to_owned();
        assert_eq!(
            vec!["os", "available_parallelism"],
            host.differences(&other)
        );
    }

    #[test]
    fn memory_sizes_of_one_gib_are_no_difference() {
        let host = |bytes| Fingerprint {
            arch: "x86_64".to_owned(),
            os: "linux".to_owned(),
            available_parallelism: Some(4),
            cpu_model: Some("AMD EPYC 9V74 80-Core Processor".to_owned()),
            total_memory_bytes: Some(bytes),
        };
        let one = host(16_760_340_480);
        assert_eq!(Vec::<&str>::new(), one.differences(&host(16_760_344_576)));
        assert_eq!(
            vec!["total_memory_bytes"],
            one.differences(&host(8_317_927_424))
        );
    }
}

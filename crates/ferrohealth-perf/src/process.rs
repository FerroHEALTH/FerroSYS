// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The CPU time and memory of the server under test, read from `/proc` or
//! from its container's cgroup.
//!
//! A process is read from `/proc/<pid>/stat` (user and system time, every
//! thread included) and `/proc/<pid>/status` (`VmRSS`, the resident set)
//! (Linux `proc(5)`). A container is read from its cgroup v2 directory:
//! `cpu.stat` `usage_usec`, and `memory.current` less the `inactive_file` of
//! `memory.stat`, the working set (Linux, "Control Group v2", Memory and CPU
//! interface files). The Docker socket is never asked. On a system without
//! `/proc` both readings are unavailable, never guessed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

/// The server to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A process, by id.
    Pid(u32),
    /// A cgroup v2 directory, such as a container's.
    Cgroup(PathBuf),
}

impl Target {
    /// How a report names the target.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Pid(pid) => format!("process {pid}"),
            Self::Cgroup(path) => format!("cgroup {}", path.display()),
        }
    }
}

/// One reading of the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sample {
    /// The CPU time used since the target started, when it can be read.
    pub cpu: Option<Duration>,
    /// The resident set of a process, or the working set of a cgroup, in
    /// bytes, when it can be read.
    pub memory_bytes: Option<u64>,
}

/// A target that cannot be read.
#[derive(Debug, Error)]
pub enum ProcessError {
    /// The process or cgroup does not exist.
    #[error("{0} does not exist")]
    Gone(String),
    /// A file of the target could not be read.
    #[error("cannot read {path}")]
    Read {
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: std::io::Error,
    },
    /// A file of the target does not hold what `proc(5)` or the cgroup v2
    /// interface describes.
    #[error("{path} does not hold {what}")]
    Format {
        /// The file.
        path: String,
        /// What was looked for.
        what: &'static str,
    },
    /// No cgroup of the container was found.
    #[error("no cgroup of container {id} under {root}; tried {tried}")]
    NoContainer {
        /// The container id given.
        id: String,
        /// The cgroup root searched.
        root: String,
        /// The directories searched.
        tried: String,
    },
}

/// The `AT_CLKTCK` entry of the auxiliary vector, the clock ticks per second
/// of `utime` and `stime` (Linux `getauxval(3)`).
const AT_CLKTCK: u64 = 17;

/// Reads a [`Target`].
#[derive(Debug, Clone)]
pub struct Probe {
    target: Target,
    proc_root: PathBuf,
    ticks_per_second: Option<u64>,
}

impl Probe {
    /// A probe of `target` under the system's `/proc`.
    #[must_use]
    pub fn new(target: Target) -> Self {
        Self::with_proc_root(target, PathBuf::from("/proc"))
    }

    /// A probe of `target` under the procfs at `proc_root`.
    #[must_use]
    pub fn with_proc_root(target: Target, proc_root: PathBuf) -> Self {
        let ticks_per_second = std::fs::read(proc_root.join("self/auxv"))
            .ok()
            .and_then(|auxv| clock_ticks(&auxv));
        Self {
            target,
            proc_root,
            ticks_per_second,
        }
    }

    /// The target this probe reads.
    #[must_use]
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Reads the target once.
    ///
    /// # Errors
    ///
    /// [`ProcessError::Gone`] when the process or cgroup does not exist on a
    /// system that has them, and [`ProcessError::Read`] or
    /// [`ProcessError::Format`] when one of its files cannot be read or
    /// parsed.
    pub fn sample(&self) -> Result<Sample, ProcessError> {
        match &self.target {
            Target::Pid(pid) => self.sample_pid(*pid),
            Target::Cgroup(dir) => sample_cgroup(dir),
        }
    }

    fn sample_pid(&self, pid: u32) -> Result<Sample, ProcessError> {
        // NOTE: a system without a procfs legitimately has no reading, so both stay absent.
        if !self.proc_root.is_dir() {
            return Ok(Sample::default());
        }
        let dir = self.proc_root.join(pid.to_string());
        if !dir.is_dir() {
            return Err(ProcessError::Gone(self.target.describe()));
        }
        let stat_path = dir.join("stat");
        let ticks = cpu_ticks(&read(&stat_path)?).ok_or_else(|| ProcessError::Format {
            path: stat_path.display().to_string(),
            what: "utime and stime",
        })?;
        let status_path = dir.join("status");
        let rss = resident_bytes(&read(&status_path)?).ok_or_else(|| ProcessError::Format {
            path: status_path.display().to_string(),
            what: "VmRSS",
        })?;
        let cpu = self
            .ticks_per_second
            .and_then(|per_second| u32::try_from(per_second).ok())
            .and_then(|per_second| Duration::from_secs(ticks).checked_div(per_second));
        Ok(Sample {
            cpu,
            memory_bytes: Some(rss),
        })
    }
}

fn read(path: &Path) -> Result<String, ProcessError> {
    std::fs::read_to_string(path).map_err(|source| ProcessError::Read {
        path: path.display().to_string(),
        source,
    })
}

fn sample_cgroup(dir: &Path) -> Result<Sample, ProcessError> {
    if !dir.is_dir() {
        return Err(ProcessError::Gone(format!("cgroup {}", dir.display())));
    }
    let format = |path: &Path, what| ProcessError::Format {
        path: path.display().to_string(),
        what,
    };
    let cpu_path = dir.join("cpu.stat");
    let usage =
        keyed(&read(&cpu_path)?, "usage_usec").ok_or_else(|| format(&cpu_path, "usage_usec"))?;
    let current_path = dir.join("memory.current");
    let current: u64 = read(&current_path)?
        .trim()
        .parse()
        .map_err(|_not_a_number| format(&current_path, "a byte count"))?;
    let stat_path = dir.join("memory.stat");
    let inactive = keyed(&read(&stat_path)?, "inactive_file")
        .ok_or_else(|| format(&stat_path, "inactive_file"))?;
    Ok(Sample {
        cpu: Some(Duration::from_micros(usage)),
        memory_bytes: Some(current.saturating_sub(inactive)),
    })
}

/// The value of `key` in a flat-keyed cgroup file (`key value` per line).
fn keyed(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == key).then(|| value.trim().parse().ok()).flatten()
    })
}

/// `utime` plus `stime` of a `/proc/<pid>/stat` body, in clock ticks.
///
/// The command name in field 2 may hold spaces and parentheses, so the
/// fields are counted from the last `)`: field 3 comes first there, so
/// `utime` (field 14) and `stime` (field 15) are the 12th and 13th.
fn cpu_ticks(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    utime.checked_add(stime)
}

/// The `VmRSS` of a `/proc/<pid>/status` body, in bytes.
fn resident_bytes(status: &str) -> Option<u64> {
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// The `AT_CLKTCK` value of an auxiliary vector, pairs of native words.
fn clock_ticks(auxv: &[u8]) -> Option<u64> {
    let word = size_of::<usize>();
    let value = |chunk: &[u8]| -> Option<u64> {
        match word {
            8 => chunk.try_into().ok().map(u64::from_ne_bytes),
            4 => chunk
                .try_into()
                .ok()
                .map(|bytes| u64::from(u32::from_ne_bytes(bytes))),
            _ => None,
        }
    };
    auxv.chunks_exact(word.checked_mul(2)?).find_map(|pair| {
        let (key, val) = pair.split_at(word);
        (value(key)? == AT_CLKTCK).then(|| value(val)).flatten()
    })
}

/// The cgroup v2 directory of a container under `root`, found by its full
/// or leading id, in the Docker layouts of the systemd and cgroupfs drivers
/// and the Podman layout.
///
/// # Errors
///
/// [`ProcessError::NoContainer`] when no directory, or more than one,
/// matches.
pub fn container_cgroup(root: &Path, id: &str) -> Result<PathBuf, ProcessError> {
    let candidates = [
        (root.join("system.slice"), "docker-", ".scope"),
        (root.join("system.slice"), "libpod-", ".scope"),
        (root.join("docker"), "", ""),
    ];
    let well_formed = !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit());
    let mut found = Vec::new();
    for (dir, prefix, suffix) in &candidates {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(full) = name
                .strip_prefix(prefix)
                .and_then(|n| n.strip_suffix(suffix))
            else {
                continue;
            };
            if well_formed && full.starts_with(id) && entry.path().is_dir() {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(ProcessError::NoContainer {
            id: id.to_owned(),
            root: root.display().to_string(),
            tried: candidates
                .iter()
                .map(|(dir, prefix, suffix)| format!("{}/{prefix}<id>{suffix}", dir.display()))
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "4242 (my (odd) server) S 1 4242 4242 0 -1 4194560 2000 0 3 0 \
                        150 50 0 0 20 0 9 0 1234 123456789 4000 18446744073709551615";

    #[test]
    fn the_cpu_ticks_count_past_a_command_with_parentheses() {
        assert_eq!(Some(200), cpu_ticks(STAT));
        assert_eq!(None, cpu_ticks("4242 (x) S 1"));
    }

    #[test]
    fn the_resident_set_is_read_in_kibibytes() {
        let status = "Name:\tx\nVmHWM:\t  9000 kB\nVmRSS:\t  8000 kB\n";
        assert_eq!(Some(8000 * 1024), resident_bytes(status));
        assert_eq!(None, resident_bytes("Name:\tx\n"));
    }

    #[test]
    fn the_clock_ticks_come_from_the_auxiliary_vector() {
        let word = size_of::<usize>();
        let mut auxv = Vec::new();
        for (key, value) in [(6_usize, 4096_usize), (17, 100), (0, 0)] {
            auxv.extend_from_slice(&key.to_ne_bytes());
            auxv.extend_from_slice(&value.to_ne_bytes());
        }
        assert_eq!(Some(100), clock_ticks(&auxv));
        assert_eq!(None, clock_ticks(auxv.get(..word * 2).unwrap()));
    }

    fn fake_proc(pid: u32) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(root.path().join("self")).unwrap();
        std::fs::write(dir.join("stat"), STAT).unwrap();
        std::fs::write(dir.join("status"), "VmRSS:\t 100 kB\n").unwrap();
        let mut auxv = Vec::new();
        for (key, value) in [(17_usize, 100_usize), (0, 0)] {
            auxv.extend_from_slice(&key.to_ne_bytes());
            auxv.extend_from_slice(&value.to_ne_bytes());
        }
        std::fs::write(root.path().join("self/auxv"), auxv).unwrap();
        root
    }

    #[test]
    fn a_process_is_read_from_its_procfs() {
        let root = fake_proc(4242);
        let probe = Probe::with_proc_root(Target::Pid(4242), root.path().to_owned());
        let sample = probe.sample().unwrap();
        assert_eq!(
            Some(Duration::from_secs(2)),
            sample.cpu,
            "200 ticks at 100/s"
        );
        assert_eq!(Some(102_400), sample.memory_bytes);
    }

    #[test]
    fn a_missing_process_is_gone_and_a_missing_procfs_is_unavailable() {
        let root = fake_proc(4242);
        let gone = Probe::with_proc_root(Target::Pid(7), root.path().to_owned());
        assert!(matches!(gone.sample(), Err(ProcessError::Gone(_))));
        let none = Probe::with_proc_root(Target::Pid(7), root.path().join("absent"));
        assert_eq!(Sample::default(), none.sample().unwrap());
    }

    #[test]
    fn a_cgroup_gives_its_usage_and_working_set() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cpu.stat"),
            "usage_usec 2500000\nuser_usec 1\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("memory.current"), "10000\n").unwrap();
        std::fs::write(
            dir.path().join("memory.stat"),
            "anon 1\ninactive_file 4000\n",
        )
        .unwrap();
        let sample = Probe::new(Target::Cgroup(dir.path().to_owned()))
            .sample()
            .unwrap();
        assert_eq!(Some(Duration::from_millis(2_500)), sample.cpu);
        assert_eq!(Some(6_000), sample.memory_bytes);
        std::fs::write(dir.path().join("cpu.stat"), "user_usec 1\n").unwrap();
        assert!(matches!(
            Probe::new(Target::Cgroup(dir.path().to_owned())).sample(),
            Err(ProcessError::Format {
                what: "usage_usec",
                ..
            })
        ));
    }

    #[test]
    fn a_container_is_found_by_its_leading_id_in_either_layout() {
        let root = tempfile::tempdir().unwrap();
        let id = "ab12".repeat(16);
        let systemd = root.path().join(format!("system.slice/docker-{id}.scope"));
        std::fs::create_dir_all(&systemd).unwrap();
        assert_eq!(systemd, container_cgroup(root.path(), "ab12ab").unwrap());
        assert!(matches!(
            container_cgroup(root.path(), "ffff"),
            Err(ProcessError::NoContainer { .. })
        ));
        let cgroupfs = root.path().join(format!("docker/{id}"));
        std::fs::create_dir_all(&cgroupfs).unwrap();
        assert!(
            container_cgroup(root.path(), "ab12").is_err(),
            "two matches are refused"
        );
        assert!(container_cgroup(root.path(), "../x").is_err(), "not an id");
    }
}

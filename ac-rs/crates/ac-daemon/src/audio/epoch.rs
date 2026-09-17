//! Current device-enumeration epoch, per backend name — issue #461.
//!
//! Keyed by backend **name**, not by an `AudioEngine` method, so
//! `list_calibrations` can compare stored entries without starting an
//! engine. Sampled fresh on every call and never cached: the daemon
//! outlives interface power cycles.
//!
//! The JACK probe does not map a JACK port to a card. It fingerprints every
//! `/dev/snd/controlC*` and `/dev/fw*` node, with the node's mtime as its
//! creation time: the kernel creates those nodes when a card or FireWire
//! node registers and removes them when it goes away. That sees a host
//! reboot, an interface power cycle and a driver reload, for JACK over ALSA
//! and over FFADO alike. It over-flags when an unrelated audio device
//! changes, which is the accepted direction. It cannot see a FireWire bus
//! reset that keeps its node.

use std::path::Path;

use ac_core::shared::calibration::{DeviceEpoch, DeviceNode};

/// The fake backend's device node creation time when
/// `AC_FAKE_DEVICE_EPOCH` is unset. Fixed, so every fake daemon shares one
/// epoch and existing fake tests resolve `same`.
const FAKE_DEVICE_CREATED_AT: &str = "2026-01-01T00:00:00Z";

/// Where the fake backend's pieces of the fingerprint read from. Fixed,
/// except for the node time, which the test hook moves.
const FAKE_BOOT_ID: &str = "fake-boot";
const FAKE_BOOTED_AT: &str = "2026-01-01T00:00:00Z";
const FAKE_NODE: &str = "fake:device0";

/// What to check when the JACK probe's inputs cannot be read.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const JACK_PROBE_CHECK: &str = "check: /dev and /proc readable by the daemon user";

/// The device-enumeration epoch `backend` is running in now.
pub fn current_epoch(backend: &str) -> DeviceEpoch {
    match backend {
        "jack" => jack_epoch(),
        "fake" => fake_epoch(fake_device_created_at()),
        other => DeviceEpoch::NotObservable {
            reason: format!("{other} backend has no enumeration probe"),
        },
    }
}

#[cfg(target_os = "linux")]
fn jack_epoch() -> DeviceEpoch {
    probe_linux(Path::new("/proc"), Path::new("/dev"))
}

#[cfg(not(target_os = "linux"))]
fn jack_epoch() -> DeviceEpoch {
    DeviceEpoch::NotObservable {
        reason: "jack backend: no enumeration probe on this platform".to_string(),
    }
}

/// Opt-in, fake-only test hook (#461), alongside the `AC_FAKE_*` hooks in
/// `fake/hooks.rs`: `AC_FAKE_DEVICE_EPOCH`, an RFC3339 string used as the
/// fake device node's creation time. Two fake daemons with different values
/// are in different device enumerations; unset ⇒ [`FAKE_DEVICE_CREATED_AT`].
///
/// Read on every call rather than once per process, unlike the hooks in
/// `fake/hooks.rs`: the epoch is itself re-sampled on every request.
fn fake_device_created_at() -> String {
    std::env::var("AC_FAKE_DEVICE_EPOCH").unwrap_or_else(|_| FAKE_DEVICE_CREATED_AT.to_string())
}

fn fake_epoch(created_at: String) -> DeviceEpoch {
    DeviceEpoch::Observed {
        host_boot_id: FAKE_BOOT_ID.to_string(),
        host_booted_at: FAKE_BOOTED_AT.to_string(),
        devices: vec![DeviceNode {
            node: FAKE_NODE.to_string(),
            created_at,
        }],
    }
}

/// The Linux fingerprint, read under `proc_root` and `dev_root` so a test
/// can point it at a directory it built. Any read failure, or zero nodes,
/// is `NotObservable` naming what was unreadable — never a partial epoch.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn probe_linux(proc_root: &Path, dev_root: &Path) -> DeviceEpoch {
    let not_observable = |observation: String| DeviceEpoch::NotObservable {
        reason: format!("jack backend: {observation}; {JACK_PROBE_CHECK}"),
    };

    let boot_id_path = proc_root.join("sys/kernel/random/boot_id");
    let host_boot_id = match std::fs::read_to_string(&boot_id_path) {
        Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return not_observable(format!("{} unreadable", boot_id_path.display())),
    };

    let stat_path = proc_root.join("stat");
    let btime = std::fs::read_to_string(&stat_path).ok().and_then(|s| {
        s.lines()
            .find_map(|l| l.strip_prefix("btime "))
            .and_then(|v| v.trim().parse::<i64>().ok())
    });
    let host_booted_at = match btime.and_then(|t| chrono::DateTime::from_timestamp(t, 0)) {
        Some(t) => t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        None => return not_observable(format!("no btime in {}", stat_path.display())),
    };

    let mut nodes = Vec::new();
    for (dir, prefix) in [
        (dev_root.join("snd"), "controlC"),
        (dev_root.to_path_buf(), "fw"),
    ] {
        match numbered_nodes(&dir, prefix) {
            Ok(found) => nodes.extend(found),
            Err(observation) => return not_observable(observation),
        }
    }
    if nodes.is_empty() {
        return not_observable(format!(
            "no {}/snd/controlC* or {}/fw* nodes",
            dev_root.display(),
            dev_root.display()
        ));
    }
    nodes.sort();

    let mut devices = Vec::with_capacity(nodes.len());
    for node in nodes {
        let created_at = match std::fs::metadata(&node).and_then(|m| m.modified()) {
            Ok(t) => chrono::DateTime::<chrono::Utc>::from(t)
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            Err(_) => return not_observable(format!("{} mtime unreadable", node.display())),
        };
        devices.push(DeviceNode {
            node: node.display().to_string(),
            created_at,
        });
    }

    DeviceEpoch::Observed {
        host_boot_id,
        host_booted_at,
        devices,
    }
}

/// Entries of `dir` named `<prefix><digits>`. A directory that cannot be
/// listed, or an entry that cannot be read, is an error naming `dir`: a
/// fingerprint missing one source would compare `same` across a boundary
/// confined to that source.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn numbered_nodes(dir: &Path, prefix: &str) -> Result<Vec<std::path::PathBuf>, String> {
    let unreadable = || format!("{} unreadable", dir.display());
    let entries = std::fs::read_dir(dir).map_err(|_| unreadable())?;
    let mut nodes = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| unreadable())?;
        let numbered = entry
            .file_name()
            .to_str()
            .and_then(|n| n.strip_prefix(prefix))
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()));
        if numbered {
            nodes.push(entry.path());
        }
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::shared::calibration::EnumerationCheck;

    /// A scratch directory removed on drop. `tempfile` is not a dependency
    /// of this crate, so this is the minimal stand-in.
    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let p =
                std::env::temp_dir().join(format!("ac-daemon-epoch-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            ScratchDir(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fake `/proc` + `/dev` under `root`, with the given node names.
    fn fake_host(
        root: &Path,
        boot_id: &str,
        nodes: &[&str],
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let proc_root = root.join("proc");
        let dev_root = root.join("dev");
        std::fs::create_dir_all(proc_root.join("sys/kernel/random")).unwrap();
        std::fs::create_dir_all(dev_root.join("snd")).unwrap();
        std::fs::write(
            proc_root.join("sys/kernel/random/boot_id"),
            format!("{boot_id}\n"),
        )
        .unwrap();
        std::fs::write(
            proc_root.join("stat"),
            "cpu  1 2 3\nbtime 1789551712\nprocesses 42\n",
        )
        .unwrap();
        for n in nodes {
            std::fs::write(dev_root.join(n), b"").unwrap();
        }
        (proc_root, dev_root)
    }

    #[test]
    fn probe_fingerprints_control_and_firewire_nodes_only() {
        let dir = ScratchDir::new("fingerprint");
        let (proc_root, dev_root) = fake_host(
            dir.path(),
            "boot-a",
            &["snd/controlC1", "snd/pcmC1D0p", "fw1", "fw", "fwx", "null"],
        );
        let DeviceEpoch::Observed {
            host_boot_id,
            host_booted_at,
            devices,
        } = probe_linux(&proc_root, &dev_root)
        else {
            panic!("expected an observed epoch");
        };
        assert_eq!(host_boot_id, "boot-a");
        assert_eq!(host_booted_at, "2026-09-16T09:41:52Z");
        let names: Vec<String> = devices.iter().map(|d| d.node.clone()).collect();
        assert_eq!(
            names,
            vec![
                dev_root.join("fw1").display().to_string(),
                dev_root.join("snd/controlC1").display().to_string(),
            ]
        );
    }

    /// AC7 on the probe: two samples of an untouched host are the same
    /// epoch, so a stored τ is not newly flagged within one enumeration.
    #[test]
    fn two_samples_of_an_untouched_host_are_the_same_epoch() {
        let dir = ScratchDir::new("untouched");
        let (proc_root, dev_root) = fake_host(dir.path(), "boot-a", &["snd/controlC0", "fw0"]);
        let a = probe_linux(&proc_root, &dev_root);
        let b = probe_linux(&proc_root, &dev_root);
        assert_eq!(EnumerationCheck::of(Some(&a), &b), EnumerationCheck::Same);
    }

    /// A node removed and created again is a crossed boundary even with the
    /// same path — the driver-reload and power-cycle shape.
    #[test]
    fn a_re_created_node_crosses() {
        let dir = ScratchDir::new("recreated");
        let (proc_root, dev_root) = fake_host(dir.path(), "boot-a", &["fw1"]);
        let before = probe_linux(&proc_root, &dev_root);
        let node = dev_root.join("fw1");
        std::fs::remove_file(&node).unwrap();
        std::fs::write(&node, b"").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&node)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let after = probe_linux(&proc_root, &dev_root);
        match EnumerationCheck::of(Some(&before), &after) {
            EnumerationCheck::Crossed { boundary, since } => {
                assert!(boundary.ends_with("fw1 re-created"), "{boundary}");
                assert!(since.is_some());
            }
            other => panic!("expected crossed, got {other:?}"),
        }
    }

    #[test]
    fn probe_without_nodes_is_not_observable_and_names_what_to_check() {
        let dir = ScratchDir::new("nonodes");
        let (proc_root, dev_root) = fake_host(dir.path(), "boot-a", &[]);
        match probe_linux(&proc_root, &dev_root) {
            DeviceEpoch::NotObservable { reason } => {
                assert!(reason.starts_with("jack backend: no "), "{reason}");
                assert!(reason.contains("controlC* or "), "{reason}");
                assert!(
                    reason.ends_with("; check: /dev and /proc readable by the daemon user"),
                    "{reason}"
                );
            }
            other => panic!("expected not_observable, got {other:?}"),
        }
    }

    /// One node source unreadable while the other holds a stable node is
    /// `NotObservable`, not a partial epoch. A partial epoch would read
    /// `same` across a boundary confined to the hidden source: the driver
    /// reload that re-created `controlC1` and left `fw1` untouched.
    #[test]
    fn one_unreadable_node_source_is_not_observable() {
        let dir = ScratchDir::new("partial");
        let (proc_root, dev_root) = fake_host(dir.path(), "boot-a", &["snd/controlC1", "fw1"]);
        let whole = probe_linux(&proc_root, &dev_root);
        assert!(matches!(whole, DeviceEpoch::Observed { .. }), "{whole:?}");

        // `/dev/snd` replaced by a regular file: `read_dir` fails for any
        // user, root included, so the test does not depend on permissions.
        let snd = dev_root.join("snd");
        std::fs::remove_dir_all(&snd).unwrap();
        std::fs::write(&snd, b"").unwrap();
        match probe_linux(&proc_root, &dev_root) {
            DeviceEpoch::NotObservable { reason } => {
                assert!(
                    reason.starts_with(&format!("jack backend: {} unreadable", snd.display())),
                    "{reason}"
                );
                assert!(reason.ends_with(JACK_PROBE_CHECK), "{reason}");
            }
            other => panic!("expected not_observable, got {other:?}"),
        }

        // The case the old probe got wrong: the other source is still
        // readable and non-empty, so dropping the unreadable one would have
        // left a non-empty, `fw1`-only observed epoch.
        let fw_only = numbered_nodes(&dev_root, "fw").unwrap();
        assert_eq!(fw_only, vec![dev_root.join("fw1")]);
        assert!(numbered_nodes(&snd, "controlC").is_err());

        // Missing, not just unreadable, is refused the same way.
        std::fs::remove_file(&snd).unwrap();
        match probe_linux(&proc_root, &dev_root) {
            DeviceEpoch::NotObservable { reason } => {
                assert!(
                    reason.contains(&format!("{} unreadable", snd.display())),
                    "{reason}"
                )
            }
            other => panic!("expected not_observable, got {other:?}"),
        }
    }

    #[test]
    fn probe_with_unreadable_proc_is_not_observable() {
        let dir = ScratchDir::new("noproc");
        let (_, dev_root) = fake_host(dir.path(), "boot-a", &["fw1"]);
        let missing = dir.path().join("no-proc");
        match probe_linux(&missing, &dev_root) {
            DeviceEpoch::NotObservable { reason } => {
                assert!(reason.contains("boot_id unreadable"), "{reason}")
            }
            other => panic!("expected not_observable, got {other:?}"),
        }
    }

    #[test]
    fn a_backend_without_a_probe_is_not_observable() {
        assert_eq!(
            current_epoch("cpal"),
            DeviceEpoch::NotObservable {
                reason: "cpal backend has no enumeration probe".to_string()
            }
        );
    }

    /// The fake hook's two values are two epochs; the default is one fixed
    /// epoch shared by every fake daemon.
    #[test]
    fn fake_epochs_differ_only_by_the_hooked_node_time() {
        let default = fake_epoch(FAKE_DEVICE_CREATED_AT.to_string());
        assert_eq!(
            EnumerationCheck::of(
                Some(&default),
                &fake_epoch(FAKE_DEVICE_CREATED_AT.to_string())
            ),
            EnumerationCheck::Same
        );
        let moved = fake_epoch("2026-02-01T00:00:00Z".to_string());
        assert!(matches!(
            EnumerationCheck::of(Some(&default), &moved),
            EnumerationCheck::Crossed { .. }
        ));
    }
}

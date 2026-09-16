//! Device-enumeration epoch of a stored τ — issue #461.
//!
//! A stored τ is not a property of [`super::TauConditions`] alone. On the
//! FF400 rig τ read 1711, 1727 and 1743 samples across sessions with every
//! conditions field matching, and it moved only across a host reboot or an
//! interface power cycle — never within one. So each [`super::TauEntry`]
//! records the [`DeviceEpoch`] it was measured in, and a lookup compares it
//! with the epoch observed now ([`EnumerationCheck::of`]).
//!
//! **The epoch is a flag, not part of the exact-match key.** A crossed
//! boundary never refuses a stored τ: it marks it unverified and names what
//! was observed. The comparison errs only toward flagging — a changed
//! fingerprint on an unrelated device flags a τ that did not move, and that
//! is accepted. The one under-flagging path known is a FireWire bus reset
//! that keeps its device node, which nothing here can see.
//!
//! Pure data and comparison: nothing in this module reads `/proc` or `/dev`.
//! The daemon's probe builds the current [`DeviceEpoch`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Boundary text for a changed host boot id. Daemon-written, printed
/// verbatim by clients, and deliberately free of a time or a cause.
pub const BOUNDARY_HOST_REBOOTED: &str = "host rebooted";

/// Boundary text for an unchanged boot id with a changed device-node set.
/// Says only what was observed: a driver reload leaves the same trace as a
/// reconnect, so neither is named.
pub const BOUNDARY_DEVICE_REENUMERATED: &str = "audio device re-enumerated";

/// Separator between [`BOUNDARY_DEVICE_REENUMERATED`] and the node list in
/// [`EnumerationCheck::Crossed::boundary`]. Clients split on it.
pub const BOUNDARY_NODES_SEPARATOR: &str = "; nodes: ";

/// One audio device node as observed on the host: its path and when the
/// kernel created it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceNode {
    pub node: String,
    /// RFC3339 creation time of the node. An observed timestamp, compared
    /// exactly — never with a tolerance.
    pub created_at: String,
}

/// The device-enumeration epoch a τ was measured in, or why none could be
/// observed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeviceEpoch {
    Observed {
        host_boot_id: String,
        /// RFC3339 host boot time.
        host_booted_at: String,
        devices: Vec<DeviceNode>,
    },
    /// The backend has no probe, or the probe's inputs were unreadable.
    /// `reason` names what to check, written as `<observation>` or
    /// `<observation>; check: <places>`.
    NotObservable { reason: String },
}

impl DeviceEpoch {
    /// The newest of the boot time and every node's creation time,
    /// rendered to whole seconds: the event this epoch dates from. `None`
    /// when the epoch was not observable.
    pub fn as_of(&self) -> Option<String> {
        match self {
            DeviceEpoch::NotObservable { .. } => None,
            DeviceEpoch::Observed {
                host_booted_at,
                devices,
                ..
            } => std::iter::once(host_booted_at.as_str())
                .chain(devices.iter().map(|d| d.created_at.as_str()))
                .max_by(|a, b| cmp_timestamps(a, b))
                .map(to_whole_seconds),
        }
    }

    /// Whether two samples describe one device enumeration. Two observed
    /// samples are compared exactly as [`EnumerationCheck::of`] compares
    /// them — boot id, then the node map — so the reported boot time alone
    /// never separates them. Any other pair must be identical: a probe that
    /// became (un)observable, or whose reason changed, is not shown to be
    /// the same enumeration.
    pub fn same_enumeration(&self, other: &DeviceEpoch) -> bool {
        match (self, other) {
            (DeviceEpoch::Observed { .. }, DeviceEpoch::Observed { .. }) => {
                EnumerationCheck::of(Some(self), other).is_same()
            }
            _ => self == other,
        }
    }
}

/// How a stored τ's epoch relates to the current one. Frozen into a report
/// at capture time; computed live only for `get_calibration` /
/// `list_calibrations`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EnumerationCheck {
    /// Same host boot and same device-node set.
    Same,
    /// A boundary was observed between the measurement and now. `boundary`
    /// is [`BOUNDARY_HOST_REBOOTED`], or [`BOUNDARY_DEVICE_REENUMERATED`]
    /// followed by [`BOUNDARY_NODES_SEPARATOR`] and the changed nodes.
    /// `since` is the boot time, or the newest node creation, to whole
    /// seconds; `None` when a node only disappeared.
    Crossed {
        boundary: String,
        since: Option<String>,
    },
    /// One side could not be observed, so nothing can be compared.
    NotObservable { reason: String },
    /// The stored entry predates enumeration tracking.
    NotRecorded,
}

impl EnumerationCheck {
    /// Compare a stored epoch with the current one. See the module doc for
    /// the error direction: every rule here flags rather than trusts.
    pub fn of(stored: Option<&DeviceEpoch>, current: &DeviceEpoch) -> Self {
        let Some(stored) = stored else {
            return EnumerationCheck::NotRecorded;
        };
        match (stored, current) {
            (_, DeviceEpoch::NotObservable { reason })
            | (DeviceEpoch::NotObservable { reason }, _) => EnumerationCheck::NotObservable {
                reason: reason.clone(),
            },
            (
                DeviceEpoch::Observed {
                    host_boot_id: stored_boot,
                    devices: stored_devices,
                    ..
                },
                DeviceEpoch::Observed {
                    host_boot_id: current_boot,
                    host_booted_at,
                    devices: current_devices,
                },
            ) => {
                if stored_boot != current_boot {
                    return EnumerationCheck::Crossed {
                        boundary: BOUNDARY_HOST_REBOOTED.to_string(),
                        since: Some(to_whole_seconds(host_booted_at)),
                    };
                }
                device_set_check(stored_devices, current_devices)
            }
        }
    }

    /// `true` only for [`EnumerationCheck::Same`].
    pub fn is_same(&self) -> bool {
        matches!(self, EnumerationCheck::Same)
    }
}

/// Same boot: compare the node sets by path and creation time.
fn device_set_check(stored: &[DeviceNode], current: &[DeviceNode]) -> EnumerationCheck {
    let stored_map: BTreeMap<&str, &str> = stored
        .iter()
        .map(|d| (d.node.as_str(), d.created_at.as_str()))
        .collect();
    let current_map: BTreeMap<&str, &str> = current
        .iter()
        .map(|d| (d.node.as_str(), d.created_at.as_str()))
        .collect();
    if stored_map == current_map {
        return EnumerationCheck::Same;
    }

    let mut new = Vec::new();
    let mut recreated = Vec::new();
    let mut since: Option<&str> = None;
    for (&node, &created_at) in &current_map {
        match stored_map.get(node) {
            Some(&old) if old == created_at => continue,
            Some(_) => recreated.push(format!("{node} re-created")),
            None => new.push(format!("{node} new")),
        }
        since = match since {
            Some(s) if cmp_timestamps(s, created_at).is_ge() => Some(s),
            _ => Some(created_at),
        };
    }
    let gone = stored_map
        .keys()
        .filter(|node| !current_map.contains_key(*node))
        .map(|node| format!("{node} gone"));
    let list: Vec<String> = new.into_iter().chain(recreated).chain(gone).collect();

    EnumerationCheck::Crossed {
        boundary: format!(
            "{BOUNDARY_DEVICE_REENUMERATED}{BOUNDARY_NODES_SEPARATOR}{}",
            list.join(", ")
        ),
        since: since.map(to_whole_seconds),
    }
}

/// Order two RFC3339 timestamps by instant, falling back to text order for
/// anything that does not parse.
fn cmp_timestamps(a: &str, b: &str) -> std::cmp::Ordering {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

/// `2026-09-16T00:08:31.123456789Z` → `2026-09-16T00:08:31Z`. Text that
/// does not parse is returned unchanged rather than invented.
fn to_whole_seconds(ts: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(t) => t
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        Err(_) => ts.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::{DeviceEpoch, DeviceNode};

    /// An observed epoch with one boot id and `nodes` as `(path, created_at)`.
    pub(crate) fn observed(boot_id: &str, nodes: &[(&str, &str)]) -> DeviceEpoch {
        DeviceEpoch::Observed {
            host_boot_id: boot_id.to_string(),
            host_booted_at: "2026-09-15T08:00:00Z".to_string(),
            devices: nodes
                .iter()
                .map(|(node, created_at)| DeviceNode {
                    node: node.to_string(),
                    created_at: created_at.to_string(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::observed;
    use super::*;

    const T1: &str = "2026-09-15T08:00:05.100000000Z";
    const T2: &str = "2026-09-16T00:08:31.250000000Z";

    #[test]
    fn missing_stored_epoch_is_not_recorded_never_same() {
        let now = observed("boot-a", &[("/dev/fw1", T1)]);
        assert_eq!(
            EnumerationCheck::of(None, &now),
            EnumerationCheck::NotRecorded
        );
    }

    #[test]
    fn identical_epochs_are_same() {
        let a = observed("boot-a", &[("/dev/fw1", T1), ("/dev/snd/controlC1", T1)]);
        let b = observed("boot-a", &[("/dev/snd/controlC1", T1), ("/dev/fw1", T1)]);
        assert_eq!(EnumerationCheck::of(Some(&a), &b), EnumerationCheck::Same);
    }

    /// Test A on pupu: a host reboot. The boundary carries no node list.
    #[test]
    fn a_changed_boot_id_is_a_host_reboot_since_the_boot_time() {
        let stored = observed("boot-a", &[("/dev/fw1", T1)]);
        let mut now = observed("boot-b", &[("/dev/fw1", T2)]);
        if let DeviceEpoch::Observed { host_booted_at, .. } = &mut now {
            *host_booted_at = "2026-09-16T13:41:52Z".to_string();
        }
        assert_eq!(
            EnumerationCheck::of(Some(&stored), &now),
            EnumerationCheck::Crossed {
                boundary: "host rebooted".to_string(),
                since: Some("2026-09-16T13:41:52Z".to_string()),
            }
        );
    }

    /// Test B on pupu: an interface power cycle with the host up moved the
    /// node fw2 → fw1, and the control node was re-created. Pins the
    /// boundary wording, node order (new, re-created, gone) and `since`.
    #[test]
    fn a_changed_node_set_names_the_nodes_and_the_newest_creation() {
        let stored = observed("boot-a", &[("/dev/fw2", T1), ("/dev/snd/controlC1", T1)]);
        let now = observed("boot-a", &[("/dev/fw1", T2), ("/dev/snd/controlC1", T2)]);
        assert_eq!(
            EnumerationCheck::of(Some(&stored), &now),
            EnumerationCheck::Crossed {
                boundary: "audio device re-enumerated; nodes: /dev/fw1 new, \
                           /dev/snd/controlC1 re-created, /dev/fw2 gone"
                    .to_string(),
                since: Some("2026-09-16T00:08:31Z".to_string()),
            }
        );
    }

    /// A driver reload re-creates a node in place. Seconds-level text would
    /// hide a reload inside the same second; the comparison is on the full
    /// timestamp.
    #[test]
    fn a_node_re_created_within_the_same_second_still_crosses() {
        let stored = observed(
            "boot-a",
            &[("/dev/snd/controlC1", "2026-09-16T00:08:31.1Z")],
        );
        let now = observed(
            "boot-a",
            &[("/dev/snd/controlC1", "2026-09-16T00:08:31.9Z")],
        );
        assert!(matches!(
            EnumerationCheck::of(Some(&stored), &now),
            EnumerationCheck::Crossed { .. }
        ));
    }

    #[test]
    fn a_node_that_only_disappeared_crosses_with_no_since() {
        let stored = observed("boot-a", &[("/dev/fw1", T1), ("/dev/fw2", T1)]);
        let now = observed("boot-a", &[("/dev/fw1", T1)]);
        assert_eq!(
            EnumerationCheck::of(Some(&stored), &now),
            EnumerationCheck::Crossed {
                boundary: "audio device re-enumerated; nodes: /dev/fw2 gone".to_string(),
                since: None,
            }
        );
    }

    #[test]
    fn an_unobservable_side_is_not_observable_with_the_current_reason_first() {
        let observed_now = observed("boot-a", &[("/dev/fw1", T1)]);
        let cpal = DeviceEpoch::NotObservable {
            reason: "cpal backend has no enumeration probe".to_string(),
        };
        let other = DeviceEpoch::NotObservable {
            reason: "stored reason".to_string(),
        };
        assert_eq!(
            EnumerationCheck::of(Some(&observed_now), &cpal),
            EnumerationCheck::NotObservable {
                reason: "cpal backend has no enumeration probe".to_string()
            }
        );
        assert_eq!(
            EnumerationCheck::of(Some(&other), &observed_now),
            EnumerationCheck::NotObservable {
                reason: "stored reason".to_string()
            }
        );
        assert_eq!(
            EnumerationCheck::of(Some(&other), &cpal),
            EnumerationCheck::NotObservable {
                reason: "cpal backend has no enumeration probe".to_string()
            }
        );
    }

    /// UX: the verdict line is `UNVERIFIED — <head> at <since>` and must fit
    /// 79 columns at a 16-column indent, so the head is capped at 26.
    #[test]
    fn boundary_heads_fit_the_verdict_line() {
        for head in [BOUNDARY_HOST_REBOOTED, BOUNDARY_DEVICE_REENUMERATED] {
            assert!(head.chars().count() <= 26, "{head}");
        }
        assert_eq!(BOUNDARY_HOST_REBOOTED, "host rebooted");
        assert_eq!(BOUNDARY_DEVICE_REENUMERATED, "audio device re-enumerated");
    }

    #[test]
    fn as_of_is_the_newest_of_boot_and_nodes_in_whole_seconds() {
        let e = observed("boot-a", &[("/dev/fw1", T2), ("/dev/fw2", T1)]);
        assert_eq!(e.as_of().as_deref(), Some("2026-09-16T00:08:31Z"));
        let bare = observed("boot-a", &[]);
        assert_eq!(bare.as_of().as_deref(), Some("2026-09-15T08:00:00Z"));
        let none = DeviceEpoch::NotObservable {
            reason: "x".to_string(),
        };
        assert_eq!(none.as_of(), None);
    }

    #[test]
    fn epoch_and_check_wire_shapes() {
        let e = observed("boot-a", &[("/dev/fw1", T1)]);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "observed");
        assert_eq!(v["devices"][0]["node"], "/dev/fw1");
        let v = serde_json::to_value(EnumerationCheck::Same).unwrap();
        assert_eq!(v, serde_json::json!({"state": "same"}));
        let v = serde_json::to_value(EnumerationCheck::Crossed {
            boundary: "host rebooted".into(),
            since: None,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"state": "crossed", "boundary": "host rebooted", "since": null})
        );
        let v = serde_json::to_value(EnumerationCheck::NotRecorded).unwrap();
        assert_eq!(v, serde_json::json!({"state": "not_recorded"}));
    }
}

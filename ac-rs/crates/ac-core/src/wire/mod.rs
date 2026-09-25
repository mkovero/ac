//! The one definition of the first-cut PUB DATA frames (#112).
//!
//! `ac-daemon` builds these structs and serialises them; `ac-scene` and
//! `ac-cli` deserialise the same structs. Before this module each side had
//! its own statement of the schema — inline `json!` in the daemon, a typed
//! subset in `ac-scene`, untyped `Value` reads in `ac-cli` — and a field
//! renamed on one side compiled cleanly and failed at runtime as a missing
//! readout. `ZMQ.md` states the same contract in prose;
//! `ac-daemon/tests/it_zmq_doc_parity.rs` holds the document to these types.
//!
//! Protocol plumbing, not analysis: neither Tier 1 nor Tier 2, serde only,
//! no sockets.
//!
//! # Schema rules
//!
//! - **Full producer schema.** Each struct names every key the daemon emits.
//!   The daemon serialises the struct, so a key the struct does not name
//!   disappears from the wire.
//! - **Consumer leniency.** Fields a consumer has always needed are required;
//!   every other field is `#[serde(default)]`, so a frame from an older
//!   daemon still parses.
//! - **Absent stays absent.** A key the daemon omits is `skip_serializing_if`;
//!   a key the daemon writes as `null` is an `Option` that serialises `null`.
//!
//! # Versioning
//!
//! Every PUB payload carries `"wire_version": <u32>` at its top level,
//! stamped at the daemon's single publish seam. [`WIRE_VERSION`] goes up for
//! any change to any PUB payload that would make a consumer built against the
//! previous version draw a wrong value or drop a frame: a removed or renamed
//! key, or a changed unit, type or meaning. A new optional key does **not**
//! bump it. [`MIN_WIRE_VERSION`] goes up only when consumers drop support for
//! an old version. This mirrors `measurement::report`'s `SCHEMA_VERSION` /
//! `MIN_SCHEMA_VERSION`, which keeps governing the report body.

pub mod monitor;
pub mod transfer;

pub use monitor::{LoudnessFrame, SpectrumFrame};
pub use transfer::{IrFrame, MtwColumns, MtwStage, TransferFrame, WireDrive};

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// The wire contract version this build publishes and reads. v1 is the
/// contract `ZMQ.md` states as of #112.
pub const WIRE_VERSION: u32 = 1;

/// The oldest wire version this build still reads.
pub const MIN_WIRE_VERSION: u32 = 1;

/// The top-level key [`WIRE_VERSION`] travels under.
pub const WIRE_VERSION_KEY: &str = "wire_version";

/// Stamp [`WIRE_VERSION`] onto a PUB payload. A non-object payload is left
/// alone: there is nowhere to put the key, and every PUB payload the daemon
/// builds is an object.
pub fn stamp_wire_version(payload: &mut Value) {
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(WIRE_VERSION_KEY.to_string(), Value::from(WIRE_VERSION));
    }
}

/// What a refused frame's `wire_version` held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireVersionFound {
    /// An integer outside `MIN_WIRE_VERSION..=WIRE_VERSION`.
    Version(u64),
    /// Something that is not a version number at all, as its JSON text.
    Unreadable(String),
}

/// A frame whose `wire_version` this build does not read.
///
/// Distinct from a malformed frame on purpose: a version mismatch is a
/// property of the daemon build, not a transient, and the fix is a build
/// swap rather than a cable or a daemon bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireVersionError {
    pub found: WireVersionFound,
}

impl WireVersionError {
    /// The daemon's version as the operator reads it: `v2`, or the raw JSON
    /// text when it is not a number.
    pub fn found_label(&self) -> String {
        match &self.found {
            WireVersionFound::Version(n) => format!("v{n}"),
            WireVersionFound::Unreadable(raw) => raw.clone(),
        }
    }

    /// The range this build reads: `v1`, or `v1–v3` (en dash) once the range
    /// is wider than one version. Never `v1–v1`.
    pub fn supported_label() -> String {
        if MIN_WIRE_VERSION == WIRE_VERSION {
            format!("v{WIRE_VERSION}")
        } else {
            format!("v{MIN_WIRE_VERSION}\u{2013}v{WIRE_VERSION}")
        }
    }
}

impl std::fmt::Display for WireVersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "daemon sends wire {}, this build reads {}",
            self.found_label(),
            Self::supported_label()
        )
    }
}

impl std::error::Error for WireVersionError {}

/// Check a raw PUB payload's `wire_version` before typed deserialisation.
///
/// Runs on the raw value so a frame whose schema moved too far to parse is
/// still reported as a version mismatch and not as malformed.
///
/// - **Absent** is accepted: a daemon predating the field is on the v1
///   contract, and absence is not version-sniffed.
/// - A present value outside `MIN_WIRE_VERSION..=WIRE_VERSION`, or one that
///   is not an integer, is refused.
pub fn check_wire_version(frame: &Value) -> Result<(), WireVersionError> {
    let Some(v) = frame.get(WIRE_VERSION_KEY) else {
        return Ok(());
    };
    match v.as_u64() {
        Some(n) if (u64::from(MIN_WIRE_VERSION)..=u64::from(WIRE_VERSION)).contains(&n) => Ok(()),
        Some(n) => Err(WireVersionError {
            found: WireVersionFound::Version(n),
        }),
        None => Err(WireVersionError {
            found: WireVersionFound::Unreadable(v.to_string()),
        }),
    }
}

/// `deserialize_with` for a key that may be present-and-`null`: present
/// (either way) is `Some`, absent is the field's `default` of `None`. Pairs
/// with `skip_serializing_if = "Option::is_none"` so all three states
/// round-trip.
pub(crate) fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

/// `deserialize_with` for a float array whose non-finite entries the daemon
/// wrote as `null` (JSON has no NaN). `null` reads back as NaN, which
/// serialises back to `null`, so the array round-trips and a consumer that
/// filters non-finite values keeps working.
pub(crate) fn nullable_f64_vec<'de, D>(d: D) -> Result<Vec<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let v: Vec<Option<f64>> = Vec::deserialize(d)?;
    Ok(v.into_iter().map(|x| x.unwrap_or(f64::NAN)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absent_wire_version_is_accepted() {
        assert_eq!(
            check_wire_version(&json!({"type": "transfer_stream"})),
            Ok(())
        );
    }

    #[test]
    fn current_wire_version_is_accepted() {
        assert_eq!(check_wire_version(&json!({"wire_version": 1})), Ok(()));
    }

    #[test]
    fn versions_outside_the_range_are_refused_naming_both() {
        for found in [0u64, 2] {
            let err = check_wire_version(&json!({ "wire_version": found })).unwrap_err();
            assert_eq!(err.found, WireVersionFound::Version(found));
            let text = err.to_string();
            assert!(text.contains(&format!("v{found}")), "{text}");
            assert!(text.contains("reads v1"), "{text}");
        }
    }

    #[test]
    fn a_non_integer_version_is_refused_not_accepted() {
        let err = check_wire_version(&json!({"wire_version": "1"})).unwrap_err();
        assert_eq!(err.found, WireVersionFound::Unreadable("\"1\"".into()));
        assert!(check_wire_version(&json!({"wire_version": null})).is_err());
    }

    #[test]
    fn supported_label_collapses_a_one_version_range() {
        assert_eq!(
            MIN_WIRE_VERSION, WIRE_VERSION,
            "update this test's expectation"
        );
        assert_eq!(WireVersionError::supported_label(), "v1");
    }

    #[test]
    fn stamp_adds_the_version_to_an_object_only() {
        let mut v = json!({"type": "keepalive"});
        stamp_wire_version(&mut v);
        assert_eq!(v["wire_version"], json!(WIRE_VERSION));
        let mut s = json!("text");
        stamp_wire_version(&mut s);
        assert_eq!(s, json!("text"));
    }
}

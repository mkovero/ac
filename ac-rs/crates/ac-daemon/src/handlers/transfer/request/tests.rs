//! The launch contract, decided without a daemon.
//!
//! Both halves — the pair parser and [`parse_params`] — read nothing but
//! the request `Value`. That is what makes the defaults, the ranges and
//! the three rejections assertable here; every one of them used to need
//! a live daemon to reach.

use super::*;
use serde_json::json;

#[test]
fn parse_pairs_multi() {
    let cmd = json!({ "pairs": [[0, 3], [1, 3], [2, 3]] });
    assert_eq!(
        parse_transfer_pairs(&cmd).unwrap(),
        vec![(0, 3), (1, 3), (2, 3)]
    );
}

#[test]
fn parse_pairs_dedups() {
    let cmd = json!({ "pairs": [[0, 3], [1, 3], [0, 3]] });
    assert_eq!(parse_transfer_pairs(&cmd).unwrap(), vec![(0, 3), (1, 3)]);
}

#[test]
fn parse_pairs_legacy_single() {
    let cmd = json!({ "meas_channel": 0, "ref_channel": 3 });
    assert_eq!(parse_transfer_pairs(&cmd).unwrap(), vec![(0, 3)]);
}

#[test]
fn parse_pairs_empty_errors() {
    let cmd = json!({ "pairs": [] });
    assert!(parse_transfer_pairs(&cmd).is_err());
}

#[test]
fn parse_pairs_malformed_element_errors() {
    let cmd = json!({ "pairs": [[0, 3], [1]] });
    assert!(parse_transfer_pairs(&cmd).is_err());
}

#[test]
fn parse_pairs_missing_fields_errors() {
    let cmd = json!({});
    assert!(parse_transfer_pairs(&cmd).is_err());
}

/// #431: a pair element above u32 used to wrap to channel 0.
#[test]
fn parse_pairs_refuses_u32_overflow() {
    let err = parse_transfer_pairs(&json!({ "pairs": [[4294967296u64, 3]] })).unwrap_err();
    assert!(
        err.contains("pairs[0][0] is outside 0\u{2013}4294967295"),
        "{err}"
    );
    assert!(err.contains("received  4294967296"), "{err}");
    assert!(err.contains("stimulus  silent"), "{err}");
    let err = parse_transfer_pairs(&json!({ "pairs": [[0, 3], [1, -1]] })).unwrap_err();
    assert!(err.contains("pairs[1][1]"), "{err}");
    assert_eq!(
        parse_transfer_pairs(&json!({ "pairs": [[4294967295u64, 3]] })).unwrap(),
        vec![(u32::MAX, 3)]
    );
}

#[test]
fn parse_pairs_refuses_non_array_pairs() {
    let err = parse_transfer_pairs(&json!({ "pairs": 5, "meas_channel": 0, "ref_channel": 3 }))
        .unwrap_err();
    assert!(err.contains("pairs must be an array"), "{err}");
}

/// #431: the legacy single-pair form wrapped the same way.
#[test]
fn parse_pairs_legacy_refuses_u32_overflow() {
    let err = parse_transfer_pairs(&json!({ "meas_channel": 4294967296u64, "ref_channel": 3 }))
        .unwrap_err();
    assert!(err.contains("meas_channel is outside"), "{err}");
    let err = parse_transfer_pairs(&json!({ "meas_channel": 0, "ref_channel": "3" })).unwrap_err();
    assert!(err.contains("ref_channel must be an integer"), "{err}");
}
// ---- parse_params -------------------------------------------------
//
// These exist because `parse_params` reads no `ServerState` and no
// config. Every one of them used to require a live daemon to reach.

fn params(v: Value) -> Result<TransferParams, String> {
    parse_params(&v)
}

#[test]
fn params_defaults_are_passive_and_z_weighted() {
    let p = params(json!({"pairs": [[0, 1]]})).unwrap();
    assert!(!p.drive, "default session must not drive");
    assert!(!p.drivable, "default session must open no output ports");
    assert_eq!(
        p.level_dbfs,
        ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS
    );
    assert_eq!(p.weighting.tag(), "Z");
    assert_eq!(p.integration_tag, "fast");
    assert_eq!(
        p.integration_tau_s,
        ac_core::visualize::time_integration::TAU_FAST_S
    );
    assert!(p.fake_correlated_pair.is_none());
    assert!(p.fake_ring_process_secs.is_none());
}

// Legacy `drive: true` must still imply drivable, or the generator
// plays onto a port that was never opened.
#[test]
fn params_drive_implies_drivable() {
    let p = params(json!({"pairs": [[0, 1]], "drive": true})).unwrap();
    assert!(p.drivable);
}

#[test]
fn params_drivable_alone_does_not_drive() {
    let p = params(json!({"pairs": [[0, 1]], "drivable": true})).unwrap();
    assert!(p.drivable);
    assert!(!p.drive);
}

// #459: the maximum is a build constant this fn deliberately cannot see
// (see `parse_params`'s doc). It must therefore hand back exactly what
// was asked for — a check appearing here would duplicate the caller's.
#[test]
fn params_do_not_clamp_level() {
    let p = params(json!({"pairs": [[0, 1]], "level_dbfs": 0.0})).unwrap();
    assert_eq!(p.level_dbfs, 0.0);
}

// The wire contract is a strict 3-way A/C/Z. "off" is the specific
// value worth pinning: it is accepted by other weighting knobs in this
// daemon and must be refused here.
#[test]
fn params_reject_off_weighting() {
    assert!(params(json!({"pairs": [[0, 1]], "weighting": "off"})).is_err());
}

#[test]
fn params_reject_unknown_weighting() {
    let e = params(json!({"pairs": [[0, 1]], "weighting": "B"})).unwrap_err();
    assert!(e.contains("A, C, Z"), "{e}");
}

#[test]
fn params_accept_lowercase_weighting() {
    assert_eq!(
        params(json!({"pairs": [[0, 1]], "weighting": "a"}))
            .unwrap()
            .weighting
            .tag(),
        "A"
    );
}

#[test]
fn params_integration_slow_and_case_insensitive() {
    let p = params(json!({"pairs": [[0, 1]], "integration": "SLOW"})).unwrap();
    assert_eq!(p.integration_tag, "slow", "tag is normalised for the wire");
    assert_eq!(
        p.integration_tau_s,
        ac_core::visualize::time_integration::TAU_SLOW_S
    );
}

#[test]
fn params_reject_unknown_integration() {
    assert!(params(json!({"pairs": [[0, 1]], "integration": "medium"})).is_err());
}

// Out-of-range ladder knobs fall back to the default rather than
// erroring. That is the pre-existing contract; this test is here so a
// future tightening to a rejection is a visible decision rather than a
// silent one.
#[test]
fn params_out_of_range_ladder_knobs_fall_back() {
    let p = params(json!({
        "pairs": [[0, 1]],
        "mtw_ppo": 10_000.0,
        "mtw_n_blocks": 0,
    }))
    .unwrap();
    assert_eq!(p.mtw_ppo, ac_core::visualize::mtw::ladder::P_REF);
    assert_eq!(
        p.mtw_n_blocks,
        ac_core::visualize::mtw::average::DEFAULT_N_BLOCKS
    );
}

#[test]
fn params_in_range_ladder_knobs_are_taken() {
    let p = params(json!({"pairs": [[0, 1]], "mtw_ppo": 24.0, "mtw_n_blocks": 8})).unwrap();
    assert_eq!(p.mtw_ppo, 24.0);
    assert_eq!(p.mtw_n_blocks, 8);
}

// Presence of the key selects ring mode; its absence leaves the
// on-demand generator in place. An empty object is still presence.
#[test]
fn params_fake_ring_presence_selects_mode() {
    let p = params(json!({"pairs": [[0, 1]], "fake_ring": {}})).unwrap();
    assert_eq!(p.fake_ring_process_secs, Some(0.005));
    assert_eq!(p.fake_ring_period, 1024);
}

#[test]
fn params_pair_error_propagates() {
    assert!(params(json!({"pairs": []})).is_err());
}

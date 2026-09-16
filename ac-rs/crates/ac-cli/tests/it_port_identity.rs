//! #486 — the spawn helper in `support` must refuse a daemon that is not its
//! own and retry on a fresh port pair.
//!
//! Every daemon here is asked for the same fixed pair first. Only one can
//! bind it; the others' daemons die on bind while the incumbent answers their
//! readiness probe. With the pid/HOME check in place they retry and end up on
//! their own ports. Without it they accept the incumbent's reply, report the
//! incumbent's ports, and the distinct-port and retry assertions below fail.
//!
//! Kept in its own binary so a failure here cannot be mistaken for a
//! behaviour test, and one test so the sequence below is deterministic.

mod support;

use std::fs;

#[test]
fn spawns_forced_onto_one_pair_each_end_up_with_their_own_daemon() {
    let homes: Vec<_> = (0..3)
        .map(|_| support::alloc_home("ac-cli-port-identity-it"))
        .collect();

    // X takes the pair. If something else grabbed it first, X retries, and
    // X's actual ports become the contested pair.
    let x = support::spawn_daemon(&homes[0], true, "127.0.0.1", Some(support::alloc_ports()));
    let pair = (x.ctrl, x.data);

    // Y and Z are forced onto X's pair while X still holds it.
    let y = support::spawn_daemon(&homes[1], true, "127.0.0.1", Some(pair));
    let z = support::spawn_daemon(&homes[2], true, "127.0.0.1", Some(pair));

    assert_ne!(y.ctrl, x.ctrl, "Y accepted X's ctrl port");
    assert_ne!(z.ctrl, x.ctrl, "Z accepted X's ctrl port");
    assert_ne!(y.ctrl, z.ctrl, "Y and Z share a ctrl port");

    for (name, guard, home) in [
        ("X", &x, &homes[0]),
        ("Y", &y, &homes[1]),
        ("Z", &z, &homes[2]),
    ] {
        let reply = guard
            .status()
            .unwrap_or_else(|| panic!("{name} is not answering on ctrl {}", guard.ctrl));
        assert_eq!(
            reply["pid"].as_u64(),
            Some(u64::from(guard.pid())),
            "{name}'s ctrl {} is answered by another process: {reply}",
            guard.ctrl
        );
        assert_eq!(
            reply["home"].as_str(),
            home.to_str(),
            "{name}'s ctrl {} is answered under another HOME: {reply}",
            guard.ctrl
        );
    }

    assert!(y.retries >= 1, "Y was never refused the contested pair");
    assert!(z.retries >= 1, "Z was never refused the contested pair");

    drop((x, y, z));
    for home in &homes {
        let _ = fs::remove_dir_all(home);
    }
}

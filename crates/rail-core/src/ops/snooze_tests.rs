//! `snooze.set`, `snooze.clear` and `snooze.woke` on the wire: their names and JSON, and the
//! checks a request passes before the daemon touches anything.

use serde_json::json;

use super::snooze::{
    MAX_SNOOZE_SECS, MIN_SNOOZE_SECS, SnoozeClearRequest, SnoozeSetRequest, SnoozeWoke,
};
use super::{ErrorCode, names};

const NOW: i64 = 1_760_000_000;

fn set(threads: Vec<i64>, until: i64, token: &str) -> SnoozeSetRequest {
    SnoozeSetRequest {
        account: "work".into(),
        threads,
        until,
        grace: true,
        client_token: token.into(),
    }
}

#[test]
fn the_snooze_names_on_the_wire_are_fixed() {
    assert_eq!(
        [names::SNOOZE_SET, names::SNOOZE_CLEAR, names::SNOOZE_WOKE],
        ["snooze.set", "snooze.clear", "snooze.woke"]
    );
}

#[test]
fn a_snooze_request_has_the_spec_json_and_refuses_unknown_fields() {
    let request = set(vec![3], NOW + 3_600, "tok_1");
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({"account": "work", "threads": [3], "until": NOW + 3_600, "grace": true,
               "client_token": "tok_1"})
    );
    let bare: SnoozeSetRequest = serde_json::from_value(
        json!({"account": "work", "threads": [3], "until": 5, "client_token": "t"}),
    )
    .unwrap();
    assert!(!bare.grace, "grace is off unless asked for");
    let extra = json!({"account": "work", "threads": [3], "until": 5, "client_token": "t",
                       "label": "later"});
    assert!(serde_json::from_value::<SnoozeSetRequest>(extra).is_err());
}

#[test]
fn a_snooze_request_keeps_each_thread_once_in_order() {
    assert_eq!(
        set(vec![3, 1, 3], NOW + 3_600, "t").checked(NOW).unwrap(),
        vec![3, 1]
    );
}

#[test]
fn a_snooze_request_is_refused_for_bad_threads_a_bad_token_or_a_time_out_of_range() {
    let cases: [(Vec<i64>, i64, &str); 6] = [
        (vec![], NOW + 3_600, "t"),
        ((1..=201).collect(), NOW + 3_600, "t"),
        (vec![0], NOW + 3_600, "t"),
        (vec![1], NOW + MIN_SNOOZE_SECS - 1, "t"),
        (vec![1], NOW + MAX_SNOOZE_SECS + 1, "t"),
        (vec![1], NOW + 3_600, "not safe!"),
    ];
    for (threads, until, token) in cases {
        let error = set(threads, until, token).checked(NOW).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput, "{error:?}");
    }
    assert!(
        set(vec![1], NOW + MIN_SNOOZE_SECS, "t")
            .checked(NOW)
            .is_ok()
    );
    assert!(
        set(vec![1], NOW + MAX_SNOOZE_SECS, "t")
            .checked(NOW)
            .is_ok()
    );
}

#[test]
fn a_clear_request_keeps_each_thread_once_and_refuses_bad_ids() {
    let clear = |threads: Vec<i64>| SnoozeClearRequest {
        account: "work".into(),
        threads,
    };
    assert_eq!(clear(vec![2, 2, 5]).checked().unwrap(), vec![2, 5]);
    assert!(clear(vec![]).checked().is_err());
    assert!(clear(vec![-1]).checked().is_err());
}

#[test]
fn snooze_woke_has_the_spec_json() {
    let woke = SnoozeWoke {
        account: "work".into(),
        threads: vec![7],
    };
    assert_eq!(
        serde_json::to_value(&woke).unwrap(),
        json!({"account": "work", "threads": [7]})
    );
}

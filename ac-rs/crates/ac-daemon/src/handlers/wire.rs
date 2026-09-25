//! Wire-value readers: a supplied request value is either exactly valid or
//! the whole request is refused (#431).
//!
//! Pure functions over `&Value` — no `ServerState`, no config. Every handler
//! that reads a channel number, a `u32` knob or a positional numeric array
//! — and each scalar `setup` key (#516) — takes it from here, so "invalid"
//! has one definition and one refusal
//! layout. The meaning of each wire shape:
//!
//! | wire value | reader result |
//! |---|---|
//! | field absent | `None` — caller applies its default |
//! | `channels`-style array `null` ([`opt_array`]) | `None` — caller applies its default |
//! | `pairs`-style array `null` ([`opt_non_null_array`]) | [`WireError`] |
//! | nullable scalar `null` | `Some(None)` — caller clears |
//! | channel / `u32` reader: integer in 0–4294967295 | the value |
//! | [`opt_positive_f64`]: finite number > 0 (integer or float); `null` refused | the value |
//! | [`opt_nullable_finite_f64`]: finite number (integer or float, any sign) | the value |
//! | [`opt_nullable_u64`]: integer in 0–18446744073709551615 | the value |
//! | [`bounded_distinct_u32s`]: more than `limit` entries | [`ListRefusal`] — `must list at most <limit> entries` |
//! | [`bounded_distinct_u32s`]: an entry equal to an earlier one | [`ListRefusal`] — `<field>[j] repeats <field>[i]` |
//! | a value above a ceiling ([`Problem::AtMost`]) | [`WireError`] — `must be at most <limit> <unit>` |
//! | anything else | [`WireError`] — caller refuses the whole request |
//!
//! The one thing these readers never do is drop an element or narrow a
//! value: independent element filtering shifts positional pairs, and an
//! unchecked `u64 as u32` turns 4294967296 into channel 0.

use std::borrow::Cow;
use std::collections::HashMap;

use serde_json::Value;

/// Longest `received` echo, in characters, before it is cut with `…` — a
/// hostile array must not be copied back into the reply in full.
const RECEIVED_MAX_CHARS: usize = 64;

/// Continuation indent of a refusal's trailer lines. Matches the layout
/// `ac-cli`'s `check_ack` prints under its `  error: ` prefix.
const TRAILER_INDENT: &str = "         ";

/// What was wrong with a supplied value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Problem {
    NotInteger,
    OutOfRange,
    NotFinite,
    NotPositive,
    NotNonNegativeInteger,
    NotArray,
    /// A number above a fixed ceiling (#635). `limit` is a `u32` so the
    /// text renders it exactly, with no `.0`.
    AtMost {
        limit: u32,
        unit: &'static str,
    },
    /// An array with more entries than a fixed ceiling (#635).
    TooManyEntries {
        limit: u32,
    },
    /// An array entry equal to an earlier one; `earlier` is that entry's
    /// field path, e.g. `channels[0]` (#635).
    Repeats {
        earlier: String,
    },
}

impl Problem {
    fn text(&self) -> Cow<'static, str> {
        match self {
            Problem::NotInteger => "must be an integer".into(),
            Problem::OutOfRange => "is outside 0\u{2013}4294967295".into(),
            Problem::NotFinite => "must be a finite number".into(),
            Problem::NotPositive => "must be a finite number > 0".into(),
            Problem::NotNonNegativeInteger => "must be a non-negative integer".into(),
            Problem::NotArray => "must be an array".into(),
            Problem::AtMost { limit, unit } => format!("must be at most {limit} {unit}").into(),
            Problem::TooManyEntries { limit } => {
                format!("must list at most {limit} entries").into()
            }
            Problem::Repeats { earlier } => format!("repeats {earlier}").into(),
        }
    }
}

/// One refused wire value: where it was, what was wrong, what arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WireError {
    pub(crate) field: String,
    pub(crate) problem: Problem,
    /// JSON text of the received value, already truncated.
    pub(crate) received: String,
}

impl WireError {
    /// A refusal of `v`, found at `field`, for `problem`. For checks made
    /// outside this module — a ceiling on an already-parsed value — so the
    /// `received` echo is still the value as sent.
    pub(crate) fn new(field: &str, problem: Problem, v: &Value) -> Self {
        Self {
            field: field.to_string(),
            problem,
            received: received_text(v),
        }
    }

    /// The refusal text: `<headline> — <field> <problem>`, then a
    /// `received` line and each `(label, value)` trailer, labels padded to
    /// one column.
    pub(crate) fn refusal(&self, headline: &str, trailers: &[(&str, &str)]) -> String {
        let width = trailers
            .iter()
            .map(|(label, _)| label.chars().count())
            .fold("received".len(), usize::max);
        let mut out = format!("{headline} \u{2014} {} {}", self.field, self.problem.text());
        let lines = std::iter::once(("received", self.received.as_str()))
            .chain(trailers.iter().map(|&(l, v)| (l, v)));
        for (label, value) in lines {
            out.push('\n');
            out.push_str(TRAILER_INDENT);
            out.push_str(&format!("{label:<width$}  {value}"));
        }
        out
    }
}

fn received_text(v: &Value) -> String {
    let text = v.to_string();
    if text.chars().count() <= RECEIVED_MAX_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(RECEIVED_MAX_CHARS).collect();
    cut.push('\u{2026}');
    cut
}

/// A present value that must be an integer in `0..=u32::MAX`. `field` is
/// the full path named in a refusal, e.g. `channels[2]`.
pub(crate) fn u32_value(v: &Value, field: &str) -> Result<u32, WireError> {
    if let Some(u) = v.as_u64() {
        return u32::try_from(u).map_err(|_| WireError::new(field, Problem::OutOfRange, v));
    }
    if v.as_i64().is_some() {
        // Only negative integers reach here — a non-negative one is a u64.
        return Err(WireError::new(field, Problem::OutOfRange, v));
    }
    Err(WireError::new(field, Problem::NotInteger, v))
}

/// A present value that must be a finite number.
pub(crate) fn finite_f64(v: &Value, field: &str) -> Result<f64, WireError> {
    match v.as_f64() {
        Some(x) if x.is_finite() => Ok(x),
        _ => Err(WireError::new(field, Problem::NotFinite, v)),
    }
}

/// A present value that must be a number finite as `f32` — a value finite
/// as `f64` but beyond `f32::MAX` would otherwise become infinity.
pub(crate) fn finite_f32(v: &Value, field: &str) -> Result<f32, WireError> {
    let x = finite_f64(v, field)? as f32;
    if x.is_finite() {
        Ok(x)
    } else {
        Err(WireError::new(field, Problem::NotFinite, v))
    }
}

/// Optional `u32` field of `obj`. Absent → `None`; present → exactly valid
/// or refused (`null` included: a non-nullable field has no `null`).
pub(crate) fn opt_u32(obj: &Value, field: &str) -> Result<Option<u32>, WireError> {
    obj.get(field).map(|v| u32_value(v, field)).transpose()
}

/// Optional nullable `u32` field. Absent → `None` (keep), `null` →
/// `Some(None)` (clear), integer → `Some(Some(n))` (set).
pub(crate) fn opt_nullable_u32(obj: &Value, field: &str) -> Result<Option<Option<u32>>, WireError> {
    match obj.get(field) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(v) => u32_value(v, field).map(|n| Some(Some(n))),
    }
}

/// Optional non-nullable field that must be a finite number > 0. Absent →
/// `None` (keep); anything else present, `null` included, → exactly valid or
/// refused.
pub(crate) fn opt_positive_f64(obj: &Value, field: &str) -> Result<Option<f64>, WireError> {
    match obj.get(field) {
        None => Ok(None),
        Some(v) => match v.as_f64() {
            Some(x) if x.is_finite() && x > 0.0 => Ok(Some(x)),
            _ => Err(WireError::new(field, Problem::NotPositive, v)),
        },
    }
}

/// Optional nullable finite number. Absent → `None` (keep), `null` →
/// `Some(None)` (clear), finite number → `Some(Some(x))` (set).
pub(crate) fn opt_nullable_finite_f64(
    obj: &Value,
    field: &str,
) -> Result<Option<Option<f64>>, WireError> {
    match obj.get(field) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(v) => finite_f64(v, field).map(|x| Some(Some(x))),
    }
}

/// Optional nullable non-negative integer in `0..=u64::MAX`. Absent → `None`
/// (keep), `null` → `Some(None)` (clear), integer → `Some(Some(n))` (set).
/// A float — `30.0` included — a negative, a string or anything above
/// u64 is refused.
pub(crate) fn opt_nullable_u64(obj: &Value, field: &str) -> Result<Option<Option<u64>>, WireError> {
    match obj.get(field) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(v) => v
            .as_u64()
            .map(|n| Some(Some(n)))
            .ok_or_else(|| WireError::new(field, Problem::NotNonNegativeInteger, v)),
    }
}

/// Optional array field. Absent or `null` → `None`; present and not an
/// array → refused.
pub(crate) fn opt_array<'a>(
    obj: &'a Value,
    field: &str,
) -> Result<Option<&'a Vec<Value>>, WireError> {
    match obj.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => Ok(Some(a)),
        Some(v) => Err(WireError::new(field, Problem::NotArray, v)),
    }
}

/// Optional array field with no `null` default. Absent → `None`; `null` or
/// any other non-array → refused. For fields such as `pairs`, where only
/// absence selects a fallback — the `null`-means-default rule belongs to
/// `channels` alone.
pub(crate) fn opt_non_null_array<'a>(
    obj: &'a Value,
    field: &str,
) -> Result<Option<&'a Vec<Value>>, WireError> {
    match obj.get(field) {
        None => Ok(None),
        Some(Value::Array(a)) => Ok(Some(a)),
        Some(v) => Err(WireError::new(field, Problem::NotArray, v)),
    }
}

/// Optional `u32` array. Absent or `null` → `None`; `[]` → `Some(vec![])`
/// (the caller decides what empty means); any bad element refuses the whole
/// array, naming `field[i]`.
pub(crate) fn opt_u32_array(obj: &Value, field: &str) -> Result<Option<Vec<u32>>, WireError> {
    let Some(arr) = opt_array(obj, field)? else {
        return Ok(None);
    };
    arr.iter()
        .enumerate()
        .map(|(i, v)| u32_value(v, &format!("{field}[{i}]")))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// A refused list, plus the one trailer line that locates the problem in
/// it: `entries N` for a list over its ceiling (the `received` echo is cut
/// at 64 characters, so it cannot be counted from that), `channel N` for a
/// repeat (still readable when the echo is cut before the repeat).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListRefusal {
    pub(crate) error: WireError,
    pub(crate) trailer: (&'static str, String),
}

impl ListRefusal {
    /// The refusal text: [`WireError::refusal`] with this list's trailer
    /// after `received`.
    pub(crate) fn refusal(&self, headline: &str) -> String {
        let (label, value) = &self.trailer;
        self.error.refusal(headline, &[(label, value.as_str())])
    }
}

/// A parsed `u32` list that must hold at most `limit` entries, none equal
/// to an earlier one (#635). `raw` is the list as sent, for the `received`
/// echo. Length is checked first: a long list almost always repeats too,
/// and the length refusal is the one fix that clears both. A repeat is the
/// first in list order — the lowest `j` whose value already sat at some
/// `i < j`.
pub(crate) fn bounded_distinct_u32s(
    field: &str,
    raw: &Value,
    values: &[u32],
    limit: u32,
) -> Result<(), ListRefusal> {
    if values.len() > limit as usize {
        return Err(ListRefusal {
            error: WireError::new(field, Problem::TooManyEntries { limit }, raw),
            trailer: ("entries", values.len().to_string()),
        });
    }
    let mut first_at: HashMap<u32, usize> = HashMap::with_capacity(values.len());
    for (j, &v) in values.iter().enumerate() {
        if let Some(&i) = first_at.get(&v) {
            return Err(ListRefusal {
                error: WireError::new(
                    &format!("{field}[{j}]"),
                    Problem::Repeats {
                        earlier: format!("{field}[{i}]"),
                    },
                    raw,
                ),
                trailer: ("channel", v.to_string()),
            });
        }
        first_at.insert(v, j);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(v: Value) -> Value {
        json!({ "x": v })
    }

    #[test]
    fn u32_boundary() {
        assert_eq!(
            opt_u32(&field(json!(4294967295u64)), "x"),
            Ok(Some(u32::MAX))
        );
        let e = opt_u32(&field(json!(4294967296u64)), "x").unwrap_err();
        assert_eq!(e.problem, Problem::OutOfRange);
        assert_eq!(e.received, "4294967296");
        assert_eq!(opt_u32(&field(json!(0)), "x"), Ok(Some(0)));
    }

    #[test]
    fn u32_rejects_negative_float_string_and_null() {
        let cases = [
            (json!(-1), Problem::OutOfRange),
            (json!(1.5), Problem::NotInteger),
            (json!("3"), Problem::NotInteger),
            (Value::Null, Problem::NotInteger),
        ];
        for (v, want) in cases {
            let e = opt_u32(&field(v.clone()), "x").unwrap_err();
            assert_eq!(e.problem, want, "{v}");
            assert_eq!(e.field, "x");
        }
    }

    #[test]
    fn u32_absent_is_none() {
        assert_eq!(opt_u32(&json!({}), "x"), Ok(None));
    }

    #[test]
    fn nullable_u32_three_states() {
        assert_eq!(opt_nullable_u32(&json!({}), "x"), Ok(None));
        assert_eq!(opt_nullable_u32(&field(Value::Null), "x"), Ok(Some(None)));
        assert_eq!(opt_nullable_u32(&field(json!(3)), "x"), Ok(Some(Some(3))));
        assert!(opt_nullable_u32(&field(json!("x")), "x").is_err());
        assert!(opt_nullable_u32(&field(json!(4294967296u64)), "x").is_err());
    }

    #[test]
    fn positive_f64_refuses_null_zero_negative_and_non_numbers() {
        assert_eq!(opt_positive_f64(&json!({}), "x"), Ok(None));
        assert_eq!(opt_positive_f64(&field(json!(0.775)), "x"), Ok(Some(0.775)));
        assert_eq!(opt_positive_f64(&field(json!(2)), "x"), Ok(Some(2.0)));
        for v in [
            json!(0),
            json!(0.0),
            json!(-1),
            json!("0.775"),
            Value::Null,
            json!(true),
        ] {
            let e = opt_positive_f64(&field(v.clone()), "x").unwrap_err();
            assert_eq!(
                (e.field.as_str(), e.problem),
                ("x", Problem::NotPositive),
                "{v}"
            );
        }
    }

    #[test]
    fn nullable_finite_f64_three_states() {
        assert_eq!(opt_nullable_finite_f64(&json!({}), "x"), Ok(None));
        assert_eq!(
            opt_nullable_finite_f64(&field(Value::Null), "x"),
            Ok(Some(None))
        );
        assert_eq!(
            opt_nullable_finite_f64(&field(json!(-5.5)), "x"),
            Ok(Some(Some(-5.5)))
        );
        for v in [json!("24"), json!(true), json!([])] {
            let e = opt_nullable_finite_f64(&field(v.clone()), "x").unwrap_err();
            assert_eq!(e.problem, Problem::NotFinite, "{v}");
        }
    }

    #[test]
    fn nullable_u64_refuses_negative_float_string_and_overflow() {
        assert_eq!(opt_nullable_u64(&json!({}), "x"), Ok(None));
        assert_eq!(opt_nullable_u64(&field(Value::Null), "x"), Ok(Some(None)));
        assert_eq!(opt_nullable_u64(&field(json!(0)), "x"), Ok(Some(Some(0))));
        assert_eq!(
            opt_nullable_u64(&field(json!(u64::MAX)), "x"),
            Ok(Some(Some(u64::MAX)))
        );
        let above: Value = serde_json::from_str("18446744073709551616").unwrap();
        for v in [json!(-1), json!(1.5), json!(30.0), json!("300"), above] {
            let e = opt_nullable_u64(&field(v.clone()), "x").unwrap_err();
            assert_eq!(e.problem, Problem::NotNonNegativeInteger, "{v}");
        }
    }

    #[test]
    fn u32_array_shapes() {
        assert_eq!(opt_u32_array(&json!({}), "x"), Ok(None));
        assert_eq!(opt_u32_array(&field(Value::Null), "x"), Ok(None));
        assert_eq!(opt_u32_array(&field(json!([])), "x"), Ok(Some(vec![])));
        assert_eq!(
            opt_u32_array(&field(json!([2, 4294967295u64])), "x"),
            Ok(Some(vec![2, u32::MAX]))
        );
        let e = opt_u32_array(&field(json!(3)), "x").unwrap_err();
        assert_eq!(e.problem, Problem::NotArray);
    }

    #[test]
    fn non_null_array_refuses_null() {
        assert_eq!(opt_non_null_array(&json!({}), "x"), Ok(None));
        let arr = json!({ "x": [1] });
        assert_eq!(opt_non_null_array(&arr, "x"), Ok(Some(&vec![json!(1)])));
        let e = opt_non_null_array(&field(Value::Null), "x").unwrap_err();
        assert_eq!(
            (e.problem, e.received.as_str()),
            (Problem::NotArray, "null")
        );
        let e = opt_non_null_array(&field(json!(5)), "x").unwrap_err();
        assert_eq!(e.problem, Problem::NotArray);
        // The `channels` reader keeps `null` as its default.
        assert_eq!(opt_array(&field(Value::Null), "x"), Ok(None));
    }

    #[test]
    fn u32_array_refuses_whole_array_naming_index() {
        let e = opt_u32_array(&field(json!(["bad"])), "x").unwrap_err();
        assert_eq!((e.field.as_str(), e.problem), ("x[0]", Problem::NotInteger));
        assert_eq!(e.received, "\"bad\"");
        let e = opt_u32_array(&field(json!([2, "bad"])), "x").unwrap_err();
        assert_eq!(e.field, "x[1]");
        let e = opt_u32_array(&field(json!([1, 4294967297u64])), "x").unwrap_err();
        assert_eq!((e.field.as_str(), e.problem), ("x[1]", Problem::OutOfRange));
    }

    #[test]
    fn f32_rejects_overflow_and_non_numbers() {
        assert_eq!(finite_f32(&json!(20.0), "f"), Ok(20.0));
        assert_eq!(
            finite_f32(&json!(1e39), "f").unwrap_err().problem,
            Problem::NotFinite
        );
        assert!(finite_f32(&json!("x"), "f").is_err());
        assert!(finite_f32(&Value::Null, "f").is_err());
        assert_eq!(finite_f64(&json!(1e39), "f"), Ok(1e39));
    }

    #[test]
    fn refusal_layout_matches_ux() {
        let e = opt_u32_array(&json!({"channels": ["bad"]}), "channels").unwrap_err();
        assert_eq!(
            e.refusal("generate not started", &[("stimulus", "silent")]),
            "generate not started \u{2014} channels[0] must be an integer\n\
             \x20        received  \"bad\"\n\
             \x20        stimulus  silent"
        );
        let e = opt_u32(&json!({"output_channel": 4294967296u64}), "output_channel").unwrap_err();
        assert_eq!(
            e.refusal("setup rejected", &[("config", "unchanged")]),
            "setup rejected \u{2014} output_channel is outside 0\u{2013}4294967295\n\
             \x20        received  4294967296\n\
             \x20        config    unchanged"
        );
    }

    #[test]
    fn refusal_pads_to_longest_label() {
        let e = finite_f32(&json!("x"), "gain_db[0]").unwrap_err();
        let text = e.refusal(
            "mic curve not saved",
            &[
                ("paired field", "freqs_hz[0] = 20.000 Hz"),
                ("data", "existing curve unchanged"),
            ],
        );
        assert!(text.contains("\n         received      \"x\""), "{text}");
        assert!(
            text.contains("\n         data          existing curve unchanged"),
            "{text}"
        );
    }

    /// #635: the ceiling itself passes, one more entry is refused.
    #[test]
    fn bounded_distinct_accepts_limit_and_refuses_one_more() {
        let at: Vec<u32> = (0..64).collect();
        assert_eq!(bounded_distinct_u32s("x", &json!(at), &at, 64), Ok(()));
        let over: Vec<u32> = (0..65).collect();
        let e = bounded_distinct_u32s("x", &json!(over), &over, 64).unwrap_err();
        assert_eq!(e.error.field, "x");
        assert_eq!(e.error.problem, Problem::TooManyEntries { limit: 64 });
        assert_eq!(e.trailer, ("entries", "65".to_string()));
        assert_eq!(bounded_distinct_u32s("x", &json!([]), &[], 64), Ok(()));
    }

    /// #635: length is checked before repeats, so a long list of zeros is
    /// refused for its length, and the count survives the cut echo.
    #[test]
    fn bounded_distinct_checks_length_before_repeats() {
        let zeros = vec![0u32; 100_000];
        let e = bounded_distinct_u32s("channels", &json!(zeros), &zeros, 64).unwrap_err();
        assert_eq!(
            e.refusal("monitor not started"),
            format!(
                "monitor not started \u{2014} channels must list at most 64 entries\n\
                 \x20        received  [{}0\u{2026}\n\
                 \x20        entries   100000",
                "0,".repeat(31)
            )
        );
    }

    /// #635: the first repeat in list order is reported, with both indices
    /// and the repeated value.
    #[test]
    fn bounded_distinct_reports_first_repeat() {
        let e = bounded_distinct_u32s("channels", &json!([0, 1, 0]), &[0, 1, 0], 64).unwrap_err();
        assert_eq!(
            e.refusal("monitor not started"),
            "monitor not started \u{2014} channels[2] repeats channels[0]\n\
             \x20        received  [0,1,0]\n\
             \x20        channel   0"
        );
        let e = bounded_distinct_u32s("channels", &json!([0, 0, 0]), &[0, 0, 0], 64).unwrap_err();
        assert_eq!(e.error.field, "channels[1]");
        // `[5,7,7,5]`: index 2 repeats before index 3 does.
        let e =
            bounded_distinct_u32s("channels", &json!([5, 7, 7, 5]), &[5, 7, 7, 5], 64).unwrap_err();
        assert_eq!(
            (e.error.field.as_str(), e.error.problem),
            (
                "channels[2]",
                Problem::Repeats {
                    earlier: "channels[1]".to_string()
                }
            )
        );
        assert_eq!(e.trailer, ("channel", "7".to_string()));
    }

    /// The echo is serde_json's rendering of the parsed number, so `1e300`
    /// as sent reads back as `1e+300`.
    #[test]
    fn at_most_renders_the_limit_exactly() {
        let e = WireError::new(
            "snapshot_ring_s",
            Problem::AtMost {
                limit: 300,
                unit: "s",
            },
            &json!(1e300),
        );
        assert_eq!(
            e.refusal("setup rejected", &[("config", "unchanged")]),
            "setup rejected \u{2014} snapshot_ring_s must be at most 300 s\n\
             \x20        received  1e+300\n\
             \x20        config    unchanged"
        );
    }

    #[test]
    fn received_is_truncated() {
        let long: Vec<u32> = (0..1000).collect();
        let e = WireError::new("x", Problem::NotInteger, &json!(long));
        assert_eq!(e.received.chars().count(), RECEIVED_MAX_CHARS + 1);
        assert!(e.received.ends_with('\u{2026}'));
    }
}

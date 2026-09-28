//! The two-column `<freq_hz> <gain_db>` text format shared by mic
//! calibration files (`.frd` / `.txt`) and target curves — one parser for
//! both, so a file that loads as one loads as the other.
//!
//! One pair per line, separated by whitespace or a comma (a spreadsheet's
//! CSV export). Lines starting with `*`, `#` or `;` are comments. A third
//! column (phase) is ignored. Frequencies must increase strictly and every
//! value must be finite; how many points a file may hold is the caller's
//! rule.

use anyhow::Result;

/// Parse `text` into frequencies and gains. `what` names the file in the
/// errors ("mic curve", "target curve"); `points` bounds the point count.
pub fn parse_freq_db(
    text: &str,
    what: &str,
    points: std::ops::RangeInclusive<usize>,
) -> Result<(Vec<f32>, Vec<f32>)> {
    let mut freqs: Vec<f32> = Vec::new();
    let mut gains = Vec::new();
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(['*', '#', ';']) {
            continue;
        }
        let mut cols = line
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|t| !t.is_empty());
        let (f_str, g_str) = match (cols.next(), cols.next()) {
            (Some(f), Some(g)) => (f, g),
            _ => anyhow::bail!(
                "line {}: expected `freq_hz gain_db [phase]`, got {raw:?}",
                line_no + 1
            ),
        };
        let f: f32 = f_str.parse().map_err(|e| {
            anyhow::anyhow!("line {}: failed to parse freq {f_str:?}: {e}", line_no + 1)
        })?;
        let g: f32 = g_str.parse().map_err(|e| {
            anyhow::anyhow!("line {}: failed to parse gain {g_str:?}: {e}", line_no + 1)
        })?;
        if !f.is_finite() || f <= 0.0 {
            anyhow::bail!("line {}: freq must be > 0 Hz, got {f}", line_no + 1);
        }
        if !g.is_finite() {
            anyhow::bail!("line {}: gain must be finite, got {g}", line_no + 1);
        }
        if let Some(&prev) = freqs.last() {
            if f <= prev {
                anyhow::bail!(
                    "line {}: frequencies must increase strictly (got {f} after {prev})",
                    line_no + 1
                );
            }
        }
        freqs.push(f);
        gains.push(g);
    }
    if freqs.len() < *points.start() {
        anyhow::bail!(
            "{what} too sparse: got {} points, need ≥ {}",
            freqs.len(),
            points.start()
        );
    }
    if freqs.len() > *points.end() {
        anyhow::bail!(
            "{what} too dense: got {} points, max {}",
            freqs.len(),
            points.end()
        );
    }
    Ok((freqs, gains))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_and_comma_separated_files_read_the_same() {
        let ws = "* comment\n20 6\n1000\t0\n10000  -3  45\n";
        let csv = "# comment\n20,6\n1000,0\n10000,-3,45\n";
        let a = parse_freq_db(ws, "target curve", 2..=100).unwrap();
        let b = parse_freq_db(csv, "target curve", 2..=100).unwrap();
        assert_eq!(a, b);
        assert_eq!(a, (vec![20.0, 1000.0, 10000.0], vec![6.0, 0.0, -3.0]));
    }

    #[test]
    fn the_point_range_is_the_callers_and_named_in_the_error() {
        let text = "20 1\n200 2\n";
        assert!(parse_freq_db(text, "target curve", 2..=10).is_ok());
        let e = parse_freq_db(text, "mic curve", 16..=4096).unwrap_err();
        assert_eq!(
            e.to_string(),
            "mic curve too sparse: got 2 points, need ≥ 16"
        );
    }

    #[test]
    fn a_header_or_a_falling_frequency_is_refused_with_its_line() {
        let e = parse_freq_db("freq,dB\n20,1\n", "target curve", 1..=9).unwrap_err();
        assert!(e.to_string().starts_with("line 1:"), "{e}");
        let e = parse_freq_db("20 1\n10 2\n", "target curve", 1..=9).unwrap_err();
        assert!(e.to_string().starts_with("line 2:"), "{e}");
    }
}

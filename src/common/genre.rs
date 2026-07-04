use crate::utils::{GENRE_DELIM_AUTOMATON, GENRE_DELIM_EXCEPTION_AUTOMATON};
use aho_corasick::Match;

/// Utility function to split a genre tag string into individual genres.
/// Uses a two-pass Aho-Corasick algorithm:
/// Pass 1: Identifies exception phrases (e.g., "R&B") that should not be split.
///         Replaces them with spaces in a working buffer (same length to preserve positions).
/// Pass 2: Finds delimiter matches in the modified buffer, then filters out any that
///         overlap with exception-protected ranges. Uses original-input positions for
///         splitting since same-length replacement preserves byte offsets.
/// Returns a Vec of trimmed genre name slices from the original input.
pub fn split_genre_tag(input: &str) -> Vec<&str> {
    if input.is_empty() {
        return Vec::new();
    }

    let mut buffer = input.to_owned();
    let mut protected_ranges: Vec<(usize, usize)> = Vec::new();

    // Pass 1: find exception phrases, protect them, replace in buffer
    if let Some(exc_ac) = &*GENRE_DELIM_EXCEPTION_AUTOMATON.read().unwrap() {
        for mat in exc_ac.find_iter(input) {
            let start = mat.start();
            let end = mat.end();
            protected_ranges.push((start, end));
            let len = end - start;
            buffer.replace_range(start..end, &" ".repeat(len));
        }
    }

    // Pass 2: find delimiters in modified buffer
    let delimiter_matches: Vec<Match> = if let Some(delim_ac) = &*GENRE_DELIM_AUTOMATON.read().unwrap() {
        delim_ac.find_iter(&buffer).collect()
    } else {
        return vec![input.trim()];
    };

    if delimiter_matches.is_empty() {
        if !protected_ranges.is_empty() {
            return protected_ranges
                .iter()
                .filter_map(|&(start, end)| input.get(start..end).map(|s| s.trim()))
                .collect();
        }
        return vec![input.trim()];
    }

    // Filter delimiter matches: skip any that overlap with protected ranges.
    // Buffer positions == original positions since exceptions are same-length replacements.
    let delim_infos: Vec<(usize, usize)> = delimiter_matches
        .iter()
        .filter_map(|m| {
            let buf_start = m.start();
            let buf_end = m.end();
            let overlaps = protected_ranges.iter().any(|&(ps, pe)| {
                buf_start < pe && buf_end > ps
            });
            if overlaps {
                None
            } else {
                Some((buf_start, buf_end))
            }
        })
        .collect();

    if delim_infos.is_empty() {
        if !protected_ranges.is_empty() {
            return protected_ranges
                .iter()
                .filter_map(|&(start, end)| input.get(start..end).map(|s| s.trim()))
                .collect();
        }
        return vec![input.trim()];
    }

    // Split original input at delimiter positions
    let mut results: Vec<&str> = Vec::new();

    let first_range = 0..delim_infos[0].0;
    if !input[first_range.clone()].trim().is_empty() {
        results.push(input[first_range].trim());
    }

    for i in 1..delim_infos.len() {
        let between_range = delim_infos[i - 1].1..delim_infos[i].0;
        if !input[between_range.clone()].trim().is_empty() {
            results.push(input[between_range].trim());
        }
    }

    let last_range = delim_infos.last().unwrap().1..input.len();
    if !input[last_range.clone()].trim().is_empty() {
        results.push(input[last_range].trim());
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_no_delimiters() {
        let result = split_genre_tag("Rock");
        assert_eq!(result, vec!["Rock"]);
    }

    #[test]
    fn test_split_single_delimiter() {
        let result = split_genre_tag("Rock / Pop");
        assert_eq!(result, vec!["Rock", "Pop"]);
    }

    #[test]
    fn test_split_multiple_delimiters() {
        let result = split_genre_tag("Rock / Pop / Jazz");
        assert_eq!(result, vec!["Rock", "Pop", "Jazz"]);
    }

    #[test]
    fn test_split_semicolon() {
        let result = split_genre_tag("Rock;Pop;Jazz");
        assert_eq!(result, vec!["Rock", "Pop", "Jazz"]);
    }

    #[test]
    fn test_split_comma() {
        let result = split_genre_tag("Rock,Pop,Jazz");
        assert_eq!(result, vec!["Rock", "Pop", "Jazz"]);
    }
}

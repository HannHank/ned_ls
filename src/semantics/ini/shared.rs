use std::sync::Arc;

use crate::semantics::ini::ParsedIniSegment;

use super::prelude::*;

struct IndexedSegment {
    name: String,
    index: Option<String>,
}

/// Resolves the active context for a given submodule, considering any dynamic overrides that may have been applied.
/// TODO:refactor into context.rs
pub fn resolve_active_context<'db>(
    db: &'db dyn crate::db::Db,
    node: &SkeletonNode<'db>,
    absolute_path: &str,
    current_full_pattern: &str,
    network_state: &super::NetworkState<'_, 'db>,
) -> Arc<crate::context::ItemContext<'db>> {
    if let Some(overrides) = network_state.dynamic_contexts.get(absolute_path) {
        for ov in overrides.iter().rev() {
            if pattern_compatible(&ov.source_pattern, current_full_pattern) {
                return ov.context.clone();
            }
        }
    }
    network_state.get_cached_context(db, node)
}

/// Parses a pattern string into a vector of IndexedSegments → each segment contains a name and an
/// optional index
fn parse_indexed_segments(pattern: &str) -> Vec<IndexedSegment> {
    super::split_ini_key(pattern)
        .iter()
        .map(|part| {
            #[allow(clippy::collapsible_if)]
            if let Some(start) = part.find('[') {
                if let Some(end) = part.find(']') {
                    return IndexedSegment {
                        name: part[..start].to_string(),
                        index: Some(part[start + 1..end].to_string()),
                    };
                }
            }
            IndexedSegment {
                name: part.to_string(),
                index: None,
            }
        })
        .collect()
}

fn skip_globstar(segs: &[IndexedSegment]) -> &[IndexedSegment] {
    if segs.first().is_some_and(|s| s.name == "**") {
        &segs[1..]
    } else {
        segs
    }
}

fn parse_range(s: &str) -> Option<(i32, i32)> {
    let parts: Vec<&str> = s.split("..").collect();
    if parts.len() == 2 {
        let start = parts[0].parse::<i32>().ok()?;
        let end = parts[1].parse::<i32>().ok()?;
        Some((start, end))
    } else {
        None
    }
}

/// Checks if the override index matches the current index, considering wildcards and ranges.
fn index_matches(override_idx: &Option<String>, current_idx: &Option<String>) -> bool {
    match (override_idx, current_idx) {
        (None, None) => true,
        (Some(o), _) if o == "*" => true,
        (_, Some(c)) if c == "*" => true,
        (Some(o), Some(c)) if o == c => true,
        (Some(o), Some(c)) => {
            #[allow(clippy::collapsible_if)]
            if o.contains("..") {
                if let Some(range) = parse_range(o) {
                    if let Ok(idx) = c.parse::<i32>() {
                        return idx >= range.0 && idx <= range.1;
                    }
                    return c.contains("..");
                }
            }
            #[allow(clippy::collapsible_if)]
            if c.contains("..") {
                if let Some(range) = parse_range(c) {
                    if let Ok(idx) = o.parse::<i32>() {
                        return idx >= range.0 && idx <= range.1;
                    }
                }
            }
            false
        }
        (None, Some(_)) | (Some(_), None) => true,
    }
}

/// Checks if the override pattern is compatible with the current pattern.
pub fn pattern_compatible(override_pattern: &str, current_pattern: &str) -> bool {
    let ov_segs = parse_indexed_segments(override_pattern);
    let cur_segs = parse_indexed_segments(current_pattern);

    let ov_concrete = skip_globstar(&ov_segs);
    let cur_concrete = skip_globstar(&cur_segs);

    // those are empty, when the pattern is just a globstar, which is compatible with any other pattern → **.enableFramePreemption turns into [], because the cur_concrete pattern is ** which is stripped.
    if cur_concrete.is_empty() || ov_concrete.is_empty() {
        return true;
    }

    // not empty and if the lengths are different, they cannot be compatible
    if ov_concrete.len() != cur_concrete.len() {
        return false;
    }

    // Check if the indices match for each segment
    for (ov_seg, cur_seg) in ov_concrete.iter().zip(cur_concrete.iter()) {
        if !index_matches(&ov_seg.index, &cur_seg.index) {
            return false;
        }
    }

    true
}

/// Splits an INI key into parts, taking into account brackets and dots.
// TODO: Maybe I  should move this to TS
pub(crate) fn split_ini_key(key: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_bracket = false;

    for (i, c) in key.char_indices() {
        match c {
            '[' => in_bracket = true,
            ']' => in_bracket = false,
            '.' if !in_bracket => {
                parts.push(&key[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&key[start..]);
    parts
}

/// Builds a full pattern string from parsed INI segments, including indices if present.
pub fn build_full_pattern(segments: &[ParsedIniSegment]) -> String {
    segments
        .iter()
        .map(|s| {
            if let Some(ref idx) = s.index {
                format!("{}[{}]", s.name, idx)
            } else {
                s.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

pub fn parse_gate_suffix(s: &str) -> (String, Option<crate::hir::ned::gate_hir::GateDirection>) {
    use crate::hir::ned::gate_hir::GateDirection;
    if let Some(base) = s.strip_suffix("$o") {
        (base.to_string(), Some(GateDirection::Output))
    } else if let Some(base) = s.strip_suffix("$i") {
        (base.to_string(), Some(GateDirection::Input))
    } else {
        (s.to_string(), None)
    }
}

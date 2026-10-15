use std::sync::Arc;

use super::prelude::*;

/// Validates that a gate channel assignment is valid, including checking that the gate exists, is inout, and that the channel parameter name is valid.
// INFO: this is just a small draft since a full validation would also require to resolve custom
// channels → however it seems that opp is completely skipping the channel assignment validation
// meaning the language server is not expected to validate the channel assignment fully either. So we just check the basics for now.
pub fn validate_gate_channel_assignment<'db>(
    ctx: &mut ValidationCtx<'_, 'db>,
    network_state: &super::NetworkState<'_, 'db>,
    assignment: &IniAssignmentHir<'db>,
    segments: &[super::ParsedIniSegment],
    gate_pos: usize,
    target_param_name: &str,
) {
    use crate::hir::ned::gate_hir::GateDirection;

    let gate_segment = &segments[gate_pos];
    let submodule_segments = &segments[..gate_pos];

    // channel keyword must follow the gate access immediately
    let channel_follows = segments
        .get(gate_pos + 1)
        .map(|s| s.name == "channel")
        .unwrap_or(false);
    if !channel_follows {
        ctx.error(
            assignment.key_span.start,
            assignment.key_span.end,
            "Gate direction access ($i/$o) must be followed by 'channel' (e.g. gate$o[0].channel.param).".to_string(),
        );
        return;
    }

    let clean_pattern = if submodule_segments.is_empty() {
        network_state.name.clone()
    } else {
        submodule_segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".")
    };

    let full_pattern = super::build_full_pattern(submodule_segments);

    // Collect matching contexts
    let mut matched_contexts: Vec<Arc<crate::context::ItemContext<'db>>> = Vec::new();
    for idx in network_state.find_flattened_matches(&clean_pattern) {
        let node = &network_state.flattened_nodes[idx];
        let absolute_path = &network_state.flattened_paths[idx];
        matched_contexts.push(super::resolve_active_context(
            ctx.db,
            node,
            absolute_path,
            &full_pattern,
            network_state,
        ));
    }
    for (path, node) in &network_state.dynamic_nodes {
        if match_glob(&clean_pattern, path) {
            matched_contexts.push(resolve_active_context(
                ctx.db,
                node,
                path,
                &full_pattern,
                network_state,
            ));
        }
    }

    let is_sub_wildcard = submodule_segments
        .iter()
        .any(|s| s.name.contains('*') || s.name.contains('?'));

    if matched_contexts.is_empty() {
        let msg = if is_sub_wildcard {
            "Submodule pattern did not match any submodules in this network.".to_string()
        } else {
            "Submodule not found for gate access.".to_string()
        };
        ctx.warning(assignment.key_span.start, assignment.key_span.end, msg);
        return;
    }

    // Validate gate exists and is inout
    let gate_name = &gate_segment.name;
    let is_gate_wildcard = gate_name.contains('*') || gate_name.contains('?');
    let mut gate_found = false;

    for sub_ctx in &matched_contexts {
        if is_gate_wildcard {
            for (gname, gate_entry) in sub_ctx.gates.iter() {
                if match_glob(gate_name, gname) {
                    gate_found = true;
                    if gate_entry.item.direction != GateDirection::Inout {
                        ctx.error(
                            assignment.key_span.start,
                            assignment.key_span.end,
                            format!(
                                "Gate '{}' is not an inout gate; $i/$o can only be used on inout gates.",
                                gname
                            ),
                        );
                    }
                }
            }
        } else if let Some(gate_entry) = sub_ctx.gates.get(gate_name.as_str()) {
            gate_found = true;
            if gate_entry.item.direction != GateDirection::Inout {
                ctx.error(
                    assignment.key_span.start,
                    assignment.key_span.end,
                    format!(
                        "Gate '{}' is not an inout gate; $i/$o can only be used on inout gates.",
                        gate_name
                    ),
                );
            }
        }
    }

    if !gate_found {
        ctx.warning(
            assignment.key_span.start,
            assignment.key_span.end,
            format!("Gate '{}' not found in matched submodules.", gate_name),
        );
        return;
    }

    // Validate channel parameter name  → skip wildcards TODO: need to handle this later
    let is_param_wildcard = target_param_name.contains('*') || target_param_name.contains('?');
    if !is_param_wildcard
        && super::builtin_config::lookup_channel_param(target_param_name).is_none()
    // really just a
    // stupid check for now
    {
        ctx.warning(
            assignment.key_span.start,
            assignment.key_span.end,
            format!(
                "'{}' is not a known channel parameter. Expected one of: datarate, delay, ber, per, disabled.",
                target_param_name
            ),
        );
    }
}

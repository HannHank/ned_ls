use crate::semantics::{
    ini::{NetworkState, parse_ini_key},
    ned::shared::match_glob,
};
use tower_lsp::lsp_types::{LocationLink, Range};

/// Goto Definition for INI files
pub fn gd_ini_goto_definition(
    db: &dyn crate::db::Db,
    ini_file: crate::hir::SourceFile,
    position: tower_lsp::lsp_types::Position,
) -> Vec<LocationLink> {
    let rope = ini_file.rope(db);
    let char_offset = crate::utils::position_to_char(rope, position);
    let byte_offset = rope.char_to_byte(char_offset);

    let ini_hir = crate::hir::ini::lower_to_hir(db, ini_file);

    for section in ini_hir.sections(db) {
        // Check if the cursor is within the sections span
        for assignment in section.assignments(db) {
            if byte_offset >= assignment.key_span.start && byte_offset <= assignment.key_span.end {
                let full_key = assignment.key.text(db);
                let raw_key = rope
                    .byte_slice(assignment.key_span.start..assignment.key_span.end)
                    .to_string();
                let hovered_word = crate::utils::word_at_byte_offset(
                    &raw_key,
                    assignment.key_span.start,
                    byte_offset,
                );

                let mut diagnostics = Vec::new();
                let mut ctx = crate::semantics::shared::ValidationCtx::new(db, &mut diagnostics, 0);

                // does not take long since the semantic analysis is cached and we are basically
                // doing the same work here
                if let Some((network_word, network_span)) =
                    crate::semantics::ini::validate_network::resolve_network_word(
                        db, ini_file, *section,
                    )
                    && let Some(network_def) =
                        crate::semantics::ini::validate_network::validate_network_def(
                            db,
                            &mut ctx,
                            ini_file,
                            network_word,
                            network_span,
                            false,
                        )
                {
                    let skeleton = crate::semantics::shared::get_type_skeleton(db, network_def);
                    let mut network_state = crate::semantics::ini::NetworkState::new(
                        network_word.text(db).to_string(),
                        &skeleton.nodes,
                    );

                    crate::semantics::ini::populate_network_state(
                        db,
                        ini_file,
                        *section,
                        &mut network_state,
                    );

                    let results = get_ini_goto_definition(
                        db,
                        &network_state,
                        full_key,
                        &hovered_word,
                        network_def,
                    );

                    return results
                        .into_iter()
                        .filter_map(|(target_uri, start_byte, end_byte)| {
                            let target_input = db.get_source_file_input(&target_uri)?;
                            let target_rope = target_input.rope(db);
                            let start_pos =
                                crate::utils::byte_to_lsp_position(target_rope, start_byte)?;
                            let end_pos =
                                crate::utils::byte_to_lsp_position(target_rope, end_byte)?;
                            let target_range = Range {
                                start: start_pos,
                                end: end_pos,
                            };
                            Some(LocationLink {
                                origin_selection_range: None,
                                target_uri,
                                target_range,
                                target_selection_range: target_range,
                            })
                        })
                        .collect();
                }
            }
        }
    }

    Vec::new()
}

/// Returns a list of (target_uri, start_byte, end_byte) for the hovered word in the INI file
fn get_ini_goto_definition<'db>(
    db: &'db dyn crate::db::Db,
    network_state: &NetworkState<'_, 'db>,
    full_key: &str,
    hovered_word: &str,
    network_def: crate::db::DefIndex<'db>,
) -> Vec<(tower_lsp::lsp_types::Url, usize, usize)> {
    if hovered_word == "**" || hovered_word == "*" {
        return Vec::new();
    }

    let is_hovered_wildcard = hovered_word.contains('*') || hovered_word.contains('?');
    let (segments, target_param_name) = parse_ini_key(full_key);

    if hovered_word == network_state.name {
        let span = network_def.span(db);
        return vec![(network_def.uri(db).clone(), span.start, span.end)];
    }

    // clicked on parameter name
    if hovered_word == target_param_name {
        let clean_pattern = segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        let full_pattern = crate::semantics::ini::build_full_pattern(&segments);
        let results =
            collect_context_results(db, network_state, &clean_pattern, &full_pattern, |ctx| {
                if is_hovered_wildcard {
                    ctx.parameters
                        .iter()
                        .filter(|(name, _)| match_glob(hovered_word, name))
                        .map(|(_, e)| {
                            (
                                e.origin.uri(db).clone(),
                                e.item.span(db).start,
                                e.item.span(db).end,
                            )
                        })
                        .collect()
                } else {
                    // clicked on a specific parameter name
                    ctx.parameters
                        .get(hovered_word)
                        .map(|e| {
                            vec![(
                                e.origin.uri(db).clone(),
                                e.item.span(db).start,
                                e.item.span(db).end,
                            )]
                        })
                        .unwrap_or_default()
                }
            });
        if !results.is_empty() {
            return results;
        }
    }

    // clicked on a submodule segment
    if let Some(pos) = segments.iter().position(|s| s.name == hovered_word) {
        let parent_pattern = if pos == 0 {
            network_state.name.clone()
        } else {
            segments[0..pos]
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".")
        };
        let parent_full_pattern = crate::semantics::ini::build_full_pattern(&segments[..pos]);
        let results = collect_context_results(
            db,
            network_state,
            &parent_pattern,
            &parent_full_pattern,
            |ctx| {
                if is_hovered_wildcard {
                    ctx.submodules
                        .iter()
                        .filter(|(name, _)| match_glob(hovered_word, name))
                        .map(|(_, e)| {
                            (e.origin.uri(db).clone(), e.item.span.start, e.item.span.end)
                        })
                        .collect()
                } else {
                    // clicked on a specific submodule name
                    ctx.submodules
                        .get(hovered_word)
                        .map(|e| {
                            vec![(e.origin.uri(db).clone(), e.item.span.start, e.item.span.end)]
                        })
                        .unwrap_or_default()
                }
            },
        );
        if !results.is_empty() {
            return results;
        }
    }

    Vec::new()
}

/// Collects the results from all contexts that match the given pattern
fn collect_context_results<'db>(
    db: &'db dyn crate::db::Db,
    network_state: &NetworkState<'_, 'db>,
    pattern: &str,
    full_pattern: &str,
    mut get_entries: impl FnMut(
        &crate::context::ItemContext<'db>,
    ) -> Vec<(tower_lsp::lsp_types::Url, usize, usize)>,
) -> Vec<(tower_lsp::lsp_types::Url, usize, usize)> {
    let mut results = Vec::new();
    for_each_matched_context(db, network_state, pattern, full_pattern, |ctx| {
        results.extend(get_entries(ctx));
    });
    dedup_results(&mut results);
    results
}

/// Iterates over all contexts that match the given pattern and invokes the callback for each
fn for_each_matched_context<'db>(
    db: &'db dyn crate::db::Db,
    network_state: &NetworkState<'_, 'db>,
    pattern: &str,
    current_full_pattern: &str,
    mut callback: impl FnMut(&crate::context::ItemContext<'db>), // FnMut allows the callback to mutate its environment → cool pattern that I learned from the rust-analyzer
) {
    for (idx, node) in network_state.flattened_nodes.iter().enumerate() {
        let absolute_path = &network_state.flattened_paths[idx];
        if match_glob(pattern, absolute_path) || pattern == *absolute_path {
            for_each_override_ctx(
                db,
                node,
                absolute_path,
                current_full_pattern,
                network_state,
                &mut callback,
            );
        }
    }

    for (path, node) in &network_state.dynamic_nodes {
        if match_glob(pattern, path) || pattern == *path {
            for_each_override_ctx(
                db,
                node,
                path,
                current_full_pattern,
                network_state,
                &mut callback,
            );
        }
    }
}

/// Iterates over all contexts that match the given pattern and invokes the callback for each
fn for_each_override_ctx<'db>(
    db: &'db dyn crate::db::Db,
    node: &crate::semantics::shared::SkeletonNode<'db>,
    absolute_path: &str,
    current_full_pattern: &str,
    network_state: &NetworkState<'_, 'db>,
    callback: &mut impl FnMut(&crate::context::ItemContext<'db>),
) {
    let mut found_any = false;
    if let Some(overrides) = network_state.dynamic_contexts.get(absolute_path) {
        for ov in overrides {
            if crate::semantics::ini::shared::pattern_compatible(
                &ov.source_pattern,
                current_full_pattern,
            ) {
                callback(&ov.context);
                found_any = true;
            }
        }
    }
    if !found_any {
        let base_ctx = network_state.get_cached_context(db, node);
        callback(&base_ctx);
    }
}

/// Deduplicates the results by sorting and removing duplicates
fn dedup_results(results: &mut Vec<(tower_lsp::lsp_types::Url, usize, usize)>) {
    results.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
    results.dedup();
}

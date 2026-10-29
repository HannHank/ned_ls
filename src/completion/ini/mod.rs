use crate::completion::shared::{attach_range, replacement_range};
use crate::completion::shared::{param_doc_data, type_doc_data};
use crate::semantics::ini::{NetworkState, builtin_config, parse_ini_key};
use crate::semantics::ned::shared::match_glob;
use crate::semantics::shared::get_type_skeleton;
use ropey::Rope;
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, Position};

/// Returns completion items for the given position in an INI file.
pub fn completion(
    db: &dyn crate::db::Db,
    ini_file: crate::hir::SourceFile,
    rope: &Rope,
    position: Position,
) -> Vec<CompletionItem> {
    // figure out the current line and the text before the cursor
    let char_offset = crate::utils::position_to_char(rope, position);
    let byte_offset = rope.char_to_byte(char_offset);

    let line_idx = rope.char_to_line(char_offset);
    let line_start_char = rope.line_to_char(line_idx);
    let text_before_cursor = rope.slice(line_start_char..char_offset).to_string();
    let trimmed = text_before_cursor.trim_start();

    // value completion → after equals sign
    if let Some(key_part) = trimmed.split('=').next()
        && trimmed.contains('=')
    {
        // values are whole tokens → restart after = or ,
        let range = replacement_range(&text_before_cursor, &['=', ','], position);
        let items = value_completions(db, ini_file, key_part.trim(), byte_offset);
        return attach_range(items, range);
    }

    // key completion → before equals sign
    // keys are completed per segment → restart after each dot
    let range = replacement_range(&text_before_cursor, &['.', ' ', '\t'], position);
    let items = key_completions(db, ini_file, trimmed, byte_offset);
    attach_range(items, range)
}

/// Returns completion items for the given key in an INI file.
fn key_completions(
    db: &dyn crate::db::Db,
    ini_file: crate::hir::SourceFile,
    typed_text: &str,
    byte_offset: usize,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    // If the line is empty or just starting → offer top-level options
    if typed_text.is_empty() || !typed_text.contains('.') {
        // Global config keys
        for opt in builtin_config::GLOBAL_CONFIG {
            items.push(CompletionItem {
                label: opt.name.to_string(),
                kind: Some(CompletionItemKind::PROPERTY),
                detail: Some(opt.description.to_string()),
                ..Default::default()
            });
        }

        // Seed options have numeric placeholders
        for opt in builtin_config::SEED_OPTIONS {
            items.push(CompletionItem {
                label: opt.name.replace(['N', 'M'], "0"), // I do not want that an error is thrown →
                // therefore simply replace the placeholders with 0
                kind: Some(CompletionItemKind::PROPERTY),
                detail: Some(opt.description.to_string()),
                ..Default::default()
            });
        }

        // Prefixes for module paths
        items.push(CompletionItem {
            label: "**".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Match any submodule path".to_string()),
            ..Default::default()
        });

        // Network name if resolvable
        if let Some(network_name) = resolve_network_name(db, ini_file, byte_offset) {
            items.push(CompletionItem {
                label: network_name,
                kind: Some(CompletionItemKind::CLASS),
                detail: Some("Network name".to_string()),
                ..Default::default()
            });
        }

        return items;
    }

    // We have dots → parse what has been typed so far
    let (segments, _target) = parse_ini_key(typed_text);

    let clean_pattern = segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".");

    let Some(resolved) = resolve_network(db, ini_file, byte_offset) else {
        return items;
    };
    let mut network_state =
        NetworkState::new(resolved.network_name.clone(), &resolved.skeleton.nodes);
    populate_state_from_section(db, ini_file, byte_offset, &mut network_state);

    // If partial is empty → right after a dot → complete the next segment
    // If partial has text → typing a segment or parameter name

    let mut seen_submodules = std::collections::HashSet::new();
    let mut seen_params = std::collections::HashSet::new();

    let full_pattern = crate::semantics::ini::build_full_pattern(&segments);
    let mut matched_contexts: Vec<std::sync::Arc<crate::context::ItemContext>> = Vec::new();
    let mut seen_contexts = std::collections::HashSet::<usize>::new();

    // Collect (absolute_path, node) pairs that match, then resolve contexts
    let mut matched_nodes: Vec<(&str, &crate::semantics::shared::SkeletonNode)> = Vec::new();

    for (idx, node) in network_state.flattened_nodes.iter().enumerate() {
        let absolute_path = &network_state.flattened_paths[idx];
        if match_glob(&clean_pattern, absolute_path) || clean_pattern == *absolute_path {
            matched_nodes.push((absolute_path.as_str(), node));
        }
    }

    for (path, node) in &network_state.dynamic_nodes {
        if match_glob(&clean_pattern, path) || clean_pattern == *path {
            matched_nodes.push((path.as_str(), node));
        }
    }

    for (absolute_path, node) in &matched_nodes {
        let mut found_any = false;
        if let Some(overrides) = network_state.dynamic_contexts.get(*absolute_path) {
            for ov in overrides {
                if crate::semantics::ini::shared::pattern_compatible(
                    &ov.source_pattern,
                    &full_pattern,
                ) {
                    let ptr = std::sync::Arc::as_ptr(&ov.context) as usize;
                    if seen_contexts.insert(ptr) {
                        matched_contexts.push(ov.context.clone());
                    }
                    found_any = true;
                }
            }
        }
        if !found_any {
            let ctx = network_state.get_cached_context(db, node);
            let ptr = std::sync::Arc::as_ptr(&ctx) as usize;
            if seen_contexts.insert(ptr) {
                matched_contexts.push(ctx);
            }
        }
    }

    // Type of completion submodule
    let submodule_type = |sub_name: &str| -> Option<crate::db::DefIndex> {
        matched_nodes.iter().find_map(|(parent_path, _)| {
            let child_path = format!("{parent_path}.{sub_name}");
            network_state
                .dynamic_nodes
                .iter()
                .find(|(path, _)| *path == child_path)
                .map(|(_, node)| node)
                .or_else(|| {
                    network_state
                        .find_flattened_matches(&child_path)
                        .first()
                        .map(|&idx| &network_state.flattened_nodes[idx])
                })
                .and_then(|node| node.type_def)
        })
    };

    for ctx in &matched_contexts {
        for sub_name in ctx.submodules.keys() {
            if seen_submodules.insert(sub_name.clone()) {
                let type_def = submodule_type(sub_name);
                items.push(CompletionItem {
                    label: sub_name.clone(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some(type_def.map_or_else(
                        || "Submodule".to_string(),
                        |def| def.fqn(db).text(db).clone(),
                    )),
                    data: type_def.and_then(|def| type_doc_data(db, def)),
                    ..Default::default()
                });
            }
        }

        for (param_name, param_entry) in ctx.parameters.iter() {
            if seen_params.insert(param_name.clone()) {
                let is_param =
                    matches!(param_entry.item, crate::hir::ned::ParamBlockItem::Param(_));
                let detail = if let crate::hir::ned::ParamBlockItem::Param(p) = &param_entry.item {
                    p.ned_type(db)
                        .clone()
                        .map(|t| t.as_str().to_string())
                        .unwrap_or_else(|| "parameter".to_string())
                } else {
                    "parameter".to_string()
                };
                items.push(CompletionItem {
                    label: param_name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(detail),
                    data: is_param
                        .then(|| param_doc_data(db, param_entry.origin, param_name))
                        .flatten(),
                    ..Default::default()
                });
            }
        }

        for gate_name in ctx.gates.keys() {
            items.push(CompletionItem {
                label: gate_name.clone(),
                kind: Some(CompletionItemKind::INTERFACE),
                detail: Some("Gate".to_string()),
                ..Default::default()
            });
        }
    }

    // Per-object built-in config keys
    for opt in builtin_config::PER_OBJECT_CONFIG {
        items.push(CompletionItem {
            label: opt.name.to_string(),
            kind: Some(CompletionItemKind::PROPERTY),
            detail: Some(opt.description.to_string()),
            ..Default::default()
        });
    }

    // typename is always valid after a module path
    items.push(CompletionItem {
        label: "typename".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("Override module type".to_string()),
        ..Default::default()
    });

    items
}

/// Returns completion items for the value of a given key in an INI file.
fn value_completions(
    db: &dyn crate::db::Db,
    ini_file: crate::hir::SourceFile,
    key_text: &str,
    byte_offset: usize,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let (segments, target_param_name) = parse_ini_key(key_text.trim());

    // Built-in config value completion
    if target_param_name.contains('-') {
        let opt = builtin_config::lookup_global(&target_param_name)
            .or_else(|| builtin_config::lookup_per_object(&target_param_name));

        if let Some(config) = opt {
            // only makes sense for exhausted types like bool
            if config.value_type == builtin_config::ConfigValueType::Bool {
                items.push(simple_item("true", "bool"));
                items.push(simple_item("false", "bool"));
            }
        }
        return items;
    }

    // network → suggest all networks in the workspace
    if target_param_name == "network" && segments.is_empty() {
        return network_value_completions(db, ini_file);
    }

    // extends → suggest other config sections in this file
    if target_param_name == "extends" && segments.is_empty() {
        return extends_value_completions(db, ini_file, byte_offset);
    }

    // typename → suggest module types
    if target_param_name == "typename" {
        let indexes = crate::db::package_tree::workspace_indexes(db);
        for (name, defs) in indexes.suffix_index.iter() {
            if let Some(def) = defs.first() {
                let kind = def.kind(db);
                if matches!(
                    kind,
                    crate::db::DefKind::SimpleModule | crate::db::DefKind::CompoundModule
                ) {
                    items.push(CompletionItem {
                        label: format!("\"{}\"", name),
                        kind: Some(CompletionItemKind::CLASS),
                        detail: Some(def.fqn(db).text(db).to_string()),
                        data: type_doc_data(db, *def),
                        ..Default::default()
                    });
                }
            }
        }
        return items;
    }

    // NED parameter value completion based on type
    let clean_pattern = segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".");

    if let Some(resolved) = resolve_network(db, ini_file, byte_offset) {
        let mut network_state =
            NetworkState::new(resolved.network_name.clone(), &resolved.skeleton.nodes);
        populate_state_from_section(db, ini_file, byte_offset, &mut network_state);
        for (idx, node) in network_state.flattened_nodes.iter().enumerate() {
            let absolute_path = &network_state.flattened_paths[idx];

            if match_glob(&clean_pattern, absolute_path) {
                let ctx = network_state.get_cached_context(db, node);
                if let Some(param_entry) = ctx.parameters.get(&target_param_name) {
                    if let crate::hir::ned::ParamBlockItem::Param(p) = &param_entry.item
                        && let Some(crate::hir::ned::type_inference::NedType::Bool) = p.ned_type(db)
                    {
                        items.push(simple_item("true", "bool"));
                        items.push(simple_item("false", "bool"));
                    }
                    break;
                }
            }
        }
    }

    items
}

/// Suggests all networks in the workspace as values for the network key.
/// Files own packages are suggested as simple names and otherwise fqns.
fn network_value_completions(
    db: &dyn crate::db::Db,
    ini_file: crate::hir::SourceFile,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    // expected package based on the ini file location
    let expected_pkg = ini_file
        .uri(db)
        .to_file_path()
        .ok()
        .map(|path| {
            let dir = path.parent().unwrap_or(&path);
            let resolved = crate::semantics::ned::determine_expected_package(
                db,
                crate::hir::shared::FilePath::new(db, dir.to_path_buf()),
            );
            match resolved {
                crate::hir::ned::package_hir::ResolvedPackage::FromPath(pkg)
                | crate::hir::ned::package_hir::ResolvedPackage::FromName(pkg) => {
                    pkg.name(db).clone()
                }
            }
        })
        .unwrap_or_default();

    let indexes = crate::db::package_tree::workspace_indexes(db);
    for (name, defs) in indexes.suffix_index.iter() {
        for def in defs {
            if *def.kind(db) != crate::db::DefKind::Network {
                continue;
            }

            let fqn = def.fqn(db).text(db).clone();
            let in_same_package = if expected_pkg.is_empty() {
                fqn == *name
            } else {
                fqn == format!("{expected_pkg}.{name}")
            };

            items.push(CompletionItem {
                label: if in_same_package {
                    name.clone()
                } else {
                    fqn.clone()
                },
                kind: Some(CompletionItemKind::CLASS),
                detail: Some(fqn),
                data: type_doc_data(db, *def),
                ..Default::default()
            });
        }
    }

    items
}

/// Suggests the other config sections of this file as values for extends.
/// Skips the current section itself  + any section whose extends chain
/// already reaches the current one.
// NOTE: opp is doing this only on the simulation start
fn extends_value_completions(
    db: &dyn crate::db::Db,
    ini_file: crate::hir::SourceFile,
    byte_offset: usize,
) -> Vec<CompletionItem> {
    let ini_hir = crate::hir::ini::lower_to_hir(db, ini_file);

    let clean = |raw: &str| {
        raw.strip_prefix("Config ")
            .unwrap_or(raw)
            .trim()
            .to_string()
    };

    let current_name = find_section_at_offset(ini_hir, db, byte_offset)
        .map(|section| clean(section.name(db).text(db)));

    let mut items = Vec::new();
    for section in ini_hir.sections(db) {
        let candidate = clean(section.name(db).text(db));

        if Some(&candidate) == current_name.as_ref() {
            continue;
        }

        // skip candidates that would create a circular extends chain
        if let Some(current) = &current_name
            && crate::semantics::ini::extends_reaches(
                db,
                ini_file,
                &candidate,
                current,
                &mut Default::default(),
            )
        {
            continue;
        }

        items.push(CompletionItem {
            label: candidate,
            kind: Some(CompletionItemKind::MODULE),
            detail: Some("Config section".to_string()),
            ..Default::default()
        });
    }

    items
}

// Helper function to create a simple completion item with a label and detail.
fn simple_item(label: &str, detail: &str) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(CompletionItemKind::VALUE),
        detail: Some(detail.to_string()),
        ..Default::default()
    }
}

/// Resolves the network name from the INI file at the given byte offset.
// just a wrapper around resolve_network to return only the name
fn resolve_network_name(
    db: &dyn crate::db::Db,
    ini_file: crate::hir::SourceFile,
    byte_offset: usize,
) -> Option<String> {
    let ini_hir = crate::hir::ini::lower_to_hir(db, ini_file);
    let section = find_section_at_offset(ini_hir, db, byte_offset)?;
    let (network_word, _) =
        crate::semantics::ini::validate_network::resolve_network_word(db, ini_file, section)?;
    Some(network_word.text(db).to_string())
}

struct ResolvedNetwork<'db> {
    network_name: String,
    skeleton: crate::semantics::shared::TypeSkeleton<'db>,
}

/// Resolves the network name and its type skeleton from the INI file at the given byte offset.
fn resolve_network<'db>(
    db: &'db dyn crate::db::Db,
    ini_file: crate::hir::SourceFile,
    byte_offset: usize,
) -> Option<ResolvedNetwork<'db>> {
    let ini_hir = crate::hir::ini::lower_to_hir(db, ini_file);
    let section = find_section_at_offset(ini_hir, db, byte_offset)?;

    let (network_word, network_span) =
        crate::semantics::ini::validate_network::resolve_network_word(db, ini_file, section)?;

    let mut diagnostics = Vec::new();
    let mut ctx = crate::semantics::shared::ValidationCtx::new(db, &mut diagnostics, 0);

    let network_def = crate::semantics::ini::validate_network::validate_network_def(
        db,
        &mut ctx,
        ini_file,
        network_word,
        network_span,
        false,
    )?;

    let skeleton = get_type_skeleton(db, network_def);

    Some(ResolvedNetwork {
        network_name: network_word.text(db).to_string(),
        skeleton: skeleton.clone(),
    })
}

/// Populates the NetworkState with overrides from the INI section at the given byte offset.
// This function is just an adapter to call the existing populate_network_state function with the
// correct parameters
fn populate_state_from_section<'db>(
    db: &'db dyn crate::db::Db,
    ini_file: crate::hir::SourceFile,
    byte_offset: usize,
    state: &mut NetworkState<'_, 'db>,
) {
    let ini_hir = crate::hir::ini::lower_to_hir(db, ini_file);
    let Some(section) = find_section_at_offset(ini_hir, db, byte_offset) else {
        return;
    };
    crate::semantics::ini::populate_network_state(db, ini_file, section, state);
}

fn find_section_at_offset<'db>(
    ini_hir: &crate::hir::ini::IniFileHir<'db>,
    db: &'db dyn crate::db::Db,
    byte_offset: usize,
) -> Option<crate::hir::ini::section_hir::IniSectionHir<'db>> {
    let sections = ini_hir.sections(db);
    let mut best = None;

    for section in sections {
        if section.header_span(db).start <= byte_offset {
            best = Some(*section);
        }
    }

    best
}

use super::shared::symbol_at;
use crate::db::Db;
use crate::goto_definition::{ResolvedDefinition, ScopeResolver};
use crate::hir;
use crate::hir::tree_utils::text_for_node;
use crate::utils;
use std::collections::HashMap;
use tower_lsp::lsp_types::{Location, Position, TextEdit, Url, WorkspaceEdit};

/// Finds all references to the symbol at the given position.
pub fn find_references(
    db: &crate::db::RootLspDatabase,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let sym = symbol_at(db, uri, position)?;
    let declaration = declaration_name_location(db, &sym.target);
    Some(scan_occurrences(
        db,
        &sym.target,
        &sym.word,
        &declaration,
        include_declaration,
    ))
}

/// Builds a WorkspaceEdit renaming the symbol at the given position.
///
/// INFO: I am currently not support wildcards
pub fn rename(
    db: &crate::db::RootLspDatabase,
    uri: &Url,
    position: Position,
    new_name: &str,
) -> std::result::Result<Option<WorkspaceEdit>, String> {
    if !is_valid_ned_identifier(new_name) {
        return Err(format!("'{}' is not a valid NED identifier.", new_name));
    }

    // reject built-in type names
    {
        let indexes = crate::db::package_tree::workspace_indexes(db);
        if let Some(defs) = indexes.suffix_index.get(new_name)
            && defs.iter().any(|d| d.uri(db).scheme() != "file")
        {
            return Err(format!("'{}' is a built-in NED type.", new_name));
        }
    }

    let Some(sym) = symbol_at(db, uri, position) else {
        return Ok(None);
    };

    if sym.word == new_name {
        return Ok(None);
    }

    let decl_uri = match &sym.target {
        ResolvedDefinition::Module(def) => def.uri(db),
        ResolvedDefinition::Member { origin, .. } => origin.uri(db),
    };
    if decl_uri.scheme() != "file" {
        return Err("Cannot rename a built-in type.".to_string());
    }

    // renaming a type must not collide with other in the same package
    if let ResolvedDefinition::Module(def) = &sym.target {
        let fqn = def.fqn(db).text(db);
        let package = fqn.rsplit_once('.').map(|(pkg, _)| pkg).unwrap_or("");
        let new_fqn = if package.is_empty() {
            new_name.to_string()
        } else {
            format!("{}.{}", package, new_name)
        };

        let indexes = crate::db::package_tree::workspace_indexes(db);
        // check if a type with the new name already exists in the same package
        if let Some(defs) = indexes.suffix_index.get(new_name)
            && defs.iter().any(|d| *d.fqn(db).text(db) == new_fqn)
        {
            return Err(format!(
                "A type named '{}' already exists in package '{}'.",
                new_name, package
            ));
        }
    }

    let declaration = declaration_name_location(db, &sym.target);
    let locations = scan_occurrences(db, &sym.target, &sym.word, &declaration, true);
    // if no occurrences were found, return None to indicate no edits are needed
    if locations.is_empty() {
        return Ok(None);
    }

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for location in locations {
        // the range of the occurrence is already known, so we can directly create a TextEdit
        changes.entry(location.uri).or_default().push(TextEdit {
            range: location.range,
            new_text: new_name.to_string(),
        });
    }

    Ok(Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    }))
}

// INFO: I am not quite happy with my solution and I am currently figuring out how I could get this
// from opp or the grammar directly
fn is_valid_ned_identifier(name: &str) -> bool {
    // TODO: I might still miss some indentifier but I try to add them later
    // Maybe i could also utilize the grammar
    const KEYWORDS: &[&str] = &[
        "allowunconnected",
        "ask",
        "bool",
        "channel",
        "channelinterface",
        "connections",
        "const",
        "default",
        "double",
        "extends",
        "false",
        "for",
        "gates",
        "if",
        "import",
        "index",
        "inout",
        "input",
        "int",
        "like",
        "module",
        "moduleinterface",
        "network",
        "object",
        "output",
        "package",
        "parameters",
        "parent",
        "simple",
        "sizeof",
        "string",
        "submodules",
        "this",
        "true",
        "typename",
        "types",
        "volatile",
        "xml",
    ];

    let mut chars = name.chars();
    let valid_syntax = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');

    // only accept if syntax is valid and not a keyword
    valid_syntax && !KEYWORDS.contains(&name)
}

/// Scans all ned and ini files for occurrences resolving to the target.
fn scan_occurrences<'db>(
    db: &'db crate::db::RootLspDatabase,
    target: &ResolvedDefinition<'db>,
    word: &str,
    declaration: &Option<(Url, hir::Span)>,
    include_declaration: bool,
) -> Vec<Location> {
    let mut locations = Vec::new();

    for file in db.get_all_source_file_inputs() {
        let file_uri = file.uri(db).clone();

        // built-in std lib and other virtual files cannot be opened by clients
        if file_uri.scheme() != "file" {
            continue;
        }

        let file_rope = file.rope(db);

        // candidate filter before walking the tree
        if !file_rope.to_string().contains(word) {
            continue;
        }

        if file_uri.path().ends_with(".ini") {
            scan_ini_file(db, file, target, word, &mut locations);
            continue;
        }
        if !file_uri.path().ends_with(".ned") {
            continue;
        }

        let Some(file_ast) = db.get_ast_for_file(&file) else {
            continue;
        };

        let file_resolver = ScopeResolver::new(db, file);

        // collect all leaf tokens that exactly match the word
        let mut stack = vec![file_ast.root_node()];
        while let Some(current) = stack.pop() {
            if current.child_count() > 0 {
                let mut cursor = current.walk();
                for child in current.children(&mut cursor) {
                    stack.push(child);
                }
                continue;
            }

            // like-expr strings
            if current.kind() == "string_literal" {
                // eprintln!("checking string literal: {:?}", text_for_node(&current, file_rope));
                if matches!(target, ResolvedDefinition::Module(_))
                    && current.end_byte() - current.start_byte() == word.len() + 2 // quotes
                    && text_for_node(&current, file_rope).as_deref()
                        == Some(format!("\"{}\"", word).as_str())
                    && has_ancestor_kind(&current, "like_expr")
                // check if it is a like expression
                {
                    let resolved = file_resolver.resolve(&current, current.start_byte(), word);
                    let is_match = resolved
                        .map(|list| list.iter().any(|r| same_resolution(db, r, target)))
                        .unwrap_or(false);
                    if is_match {
                        // range inside the quotes
                        let inner =
                            hir::Span::new(current.start_byte() + 1, current.end_byte() - 1);
                        if let Some(range) = utils::span_to_range(file_rope, inner) {
                            locations.push(Location {
                                uri: file_uri.clone(),
                                range,
                            });
                        }
                    }
                }
                continue;
            }

            if current.end_byte() - current.start_byte() != word.len() {
                // exact match
                continue;
            }
            if text_for_node(&current, file_rope).as_deref() != Some(word) {
                // exact match
                continue;
            }

            // ascend to the surrounding named node → did the same for the goto handler
            let mut named = current;
            while !named.is_named() && named.parent().is_some() {
                named = named.parent().unwrap();
            }

            let occurrence_span = hir::Span::new(current.start_byte(), current.end_byte());

            // skip the declaration itself if not requested
            if !include_declaration
                && let Some((decl_uri, decl_span)) = declaration
                && *decl_uri == file_uri
                && occurrence_span.start >= decl_span.start
                && occurrence_span.end <= decl_span.end
            {
                continue;
            }

            let resolved = file_resolver.resolve(&named, current.start_byte(), word);
            let is_match = resolved
                .map(|list| list.iter().any(|r| same_resolution(db, r, target)))
                .unwrap_or(false);

            if is_match && let Some(range) = utils::span_to_range(file_rope, occurrence_span) {
                locations.push(Location {
                    uri: file_uri.clone(),
                    range,
                });
            }
        }
    }

    locations
}

/// Scans an ini file for references to the target
// TODO: I need to write some tests for this → not sure if I am finally covering all cases
fn scan_ini_file<'db>(
    db: &'db crate::db::RootLspDatabase,
    file: hir::SourceFile,
    target: &ResolvedDefinition<'db>,
    word: &str,
    locations: &mut Vec<Location>,
) {
    let ini_hir = crate::hir::ini::lower_to_hir(db, file);
    let uri = file.uri(db).clone();
    let rope = file.rope(db);

    for section in ini_hir.sections(db) {
        if let ResolvedDefinition::Module(target_def) = target {
            // network =
            for cfg in section.configs(db) {
                if cfg.key.text(db) != "network" {
                    continue;
                }
                let raw = rope
                    .byte_slice(cfg.value_span.start..cfg.value_span.end)
                    .to_string();
                let clean = raw.trim().trim_matches(|c| c == '"' || c == '\'');
                if clean != word && !clean.ends_with(&format!(".{}", word)) {
                    continue;
                }

                // same logic as in the semantic check
                let mut dummy = Vec::new();
                let mut vctx = crate::semantics::shared::ValidationCtx::new(db, &mut dummy, 0);
                let net_word = crate::hir::Word::new(db, clean.to_string());
                let resolved = crate::semantics::ini::validate_network::validate_network_def(
                    db,
                    &mut vctx,
                    file,
                    net_word,
                    cfg.value_span,
                    false,
                );

                if resolved.is_some_and(|def| same_def(db, def, *target_def))
                    && let Some(span) = find_word_span(&raw, cfg.value_span.start, word)
                    && let Some(range) = utils::span_to_range(rope, span)
                {
                    locations.push(Location {
                        uri: uri.clone(),
                        range,
                    });
                }
            }

            // **.AutoTsn.typename =
            for assignment in section.assignments(db) {
                let (_, target_param) =
                    crate::semantics::ini::parse_ini_key(assignment.key.text(db));
                if target_param != "typename" {
                    continue;
                }
                let raw = rope
                    .byte_slice(assignment.value_span.start..assignment.value_span.end)
                    .to_string();
                let clean = raw.trim().trim_matches(|c| c == '"' || c == '\'');
                if clean != word && !clean.ends_with(&format!(".{}", word)) {
                    continue;
                }

                if resolve_type_value(db, clean).is_some_and(|def| same_def(db, def, *target_def))
                    && let Some(span) = find_word_span(&raw, assignment.value_span.start, word)
                    && let Some(range) = utils::span_to_range(rope, span)
                {
                    locations.push(Location {
                        uri: uri.clone(),
                        range,
                    });
                }
            }
        }

        // parameter keys: **.host.bla =
        if let ResolvedDefinition::Member { origin, span } = target {
            let assignments = section.assignments(db);
            let relevant: Vec<_> = assignments
                .iter()
                .filter(|a| {
                    let (segments, target_param) =
                        crate::semantics::ini::parse_ini_key(a.key.text(db));
                    // param segment at the end or a submodule name segment
                    target_param == word || segments.iter().any(|s| s.name == word)
                })
                .collect();
            if relevant.is_empty() {
                continue;
            }

            // resolve the sections network once → needed to verify that the
            // key actually refers to the target member
            let Some((network_name, skeleton)) = resolve_section_network(db, file, *section) else {
                continue;
            };
            let mut state = crate::semantics::ini::NetworkState::new(network_name, &skeleton.nodes);
            crate::semantics::ini::populate_network_state(db, file, *section, &mut state);

            for assignment in relevant {
                let (segments, target_param) =
                    crate::semantics::ini::parse_ini_key(assignment.key.text(db));
                let clean_pattern = segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");

                let matched = state.find_flattened_matches(&clean_pattern);

                // parameter segment at the end of the key: **.host.bla
                if target_param == word {
                    let is_reference = matched.iter().any(|idx| {
                        // eprintln!("checking flattened node {} for param {}", idx, word);
                        let node = &state.flattened_nodes[*idx];
                        let ctx = state.get_cached_context(db, node);
                        //
                        ctx.parameters.get(word).is_some_and(|entry| {
                            same_def(db, entry.origin, *origin) && entry.item.span(db) == *span
                        })
                    });

                    // if the target is a parameter, the key segment is not the same as the value segment, so we need to check the key span
                    if is_reference {
                        let raw = rope
                            .byte_slice(assignment.key_span.start..assignment.key_span.end)
                            .to_string();
                        if let Some(span) = find_word_span(&raw, assignment.key_span.start, word)
                            && let Some(range) = utils::span_to_range(rope, span)
                        {
                            locations.push(Location {
                                uri: uri.clone(),
                                range,
                            });
                        }
                    }
                }

                // submodule name segments inside the key: **.<sub_name>.bla
                // Align segments to the matched path elements from the end
                // backwards and check if the parent contains the submodule
                let mut segment_hits: std::collections::HashSet<usize> =
                    std::collections::HashSet::new();

                for idx in &matched {
                    let path = &state.flattened_paths[*idx];
                    let path_elems: Vec<&str> = path.split('.').collect();

                    let mut seg_pos = segments.len();
                    let mut elem_pos = path_elems.len();

                    while seg_pos > 0 && elem_pos > 0 {
                        let segment = &segments[seg_pos - 1];

                        // stop if we reach a wildcard segment, as it cannot be resolved to a specific submodule
                        if segment.name.contains("**") {
                            break;
                        }
                        seg_pos -= 1;
                        elem_pos -= 1;

                        // elem 0 is the network name, submodules start after it
                        if segment.name != word || elem_pos == 0 {
                            continue;
                        }

                        let parent_path = path_elems[..elem_pos].join(".");
                        let Some(parent_idx) =
                            state.find_flattened_matches(&parent_path).first().copied()
                        else {
                            continue;
                        };
                        let parent_node = &state.flattened_nodes[parent_idx];
                        let parent_ctx = state.get_cached_context(db, parent_node);

                        if parent_ctx.submodules.get(word).is_some_and(|entry| {
                            same_def(db, entry.origin, *origin) && entry.item.span == *span
                        }) {
                            segment_hits.insert(seg_pos);
                        }
                    }
                }

                if !segment_hits.is_empty() {
                    let raw = rope
                        .byte_slice(assignment.key_span.start..assignment.key_span.end)
                        .to_string();
                    let parts = dot_part_spans(&raw);

                    for seg_idx in segment_hits {
                        let Some((part_start, part_end)) = parts.get(seg_idx).copied() else {
                            continue;
                        };
                        // the segment name sits at the part start, optionally
                        // followed by an index like [0] or [*]
                        let boundary_ok = raw[part_start..].starts_with(word)
                            && (part_start + word.len() == part_end
                                || raw.as_bytes()[part_start + word.len()] == b'[');
                        if boundary_ok
                            && let Some(range) = utils::span_to_range(
                                rope,
                                hir::Span::new(
                                    assignment.key_span.start + part_start,
                                    assignment.key_span.start + part_start + word.len(),
                                ),
                            )
                        {
                            // eprintln!("found submodule reference in ini: {} at {:?}", raw, range);
                            locations.push(Location {
                                uri: uri.clone(),
                                range,
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Byte spans of the top-level dot separated parts of an ini key.
/// [0..4] etc
fn dot_part_spans(raw: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    let mut depth: i32 = 0;

    for (i, b) in raw.bytes().enumerate() {
        match b {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth -= 1,
            b'.' if depth == 0 => {
                spans.push((start, i));
                start = i + 1;
            }
            _ => {}
        }
    }
    spans.push((start, raw.len()));
    spans
}

/// Semantic def identity
// I previously had sometimes used DefIndex:new for some queries, creating different salsa ids for
// the same content. To be save in the future when I do it again, this function should be used.
// Otherwise == might returns a mismatch.
fn same_def(db: &dyn Db, a: crate::db::DefIndex<'_>, b: crate::db::DefIndex<'_>) -> bool {
    a.fqn(db) == b.fqn(db) && a.uri(db) == b.uri(db)
}

fn same_resolution(db: &dyn Db, a: &ResolvedDefinition<'_>, b: &ResolvedDefinition<'_>) -> bool {
    match (a, b) {
        (ResolvedDefinition::Module(x), ResolvedDefinition::Module(y)) => same_def(db, *x, *y),
        (
            ResolvedDefinition::Member {
                origin: o1,
                span: s1,
            },
            ResolvedDefinition::Member {
                origin: o2,
                span: s2,
            },
        ) => s1 == s2 && same_def(db, *o1, *o2),
        _ => false,
    }
}

/// Resolves a typename value the way process_typename_assignment does.
fn resolve_type_value<'db>(db: &'db dyn Db, clean: &str) -> Option<crate::db::DefIndex<'db>> {
    if clean.contains('.') {
        let pattern = crate::hir::ned::import_hir::ImportPattern::new(db, clean.to_string());
        crate::db::package_tree::resolve_import(db, pattern)
            .clone()
            .and_then(|defs| defs.first().copied())
    } else {
        *crate::db::package_tree::resolve_global_type(
            db,
            crate::hir::Word::new(db, clean.to_string()),
        )
    }
}

/// Network name + skeleton for a section
fn resolve_section_network<'db>(
    db: &'db dyn Db,
    ini_file: hir::SourceFile,
    section: crate::hir::ini::section_hir::IniSectionHir<'db>,
) -> Option<(String, crate::semantics::shared::TypeSkeleton<'db>)> {
    let (network_word, network_span) =
        crate::semantics::ini::validate_network::resolve_network_word(db, ini_file, section)?;

    let mut dummy = Vec::new(); // do not need this TODO: maybe I need to refactor the interface
    let mut vctx = crate::semantics::shared::ValidationCtx::new(db, &mut dummy, 0);
    let network_def = crate::semantics::ini::validate_network::validate_network_def(
        db,
        &mut vctx,
        ini_file,
        network_word,
        network_span,
        false,
    )?;

    Some((
        network_word.text(db).to_string(),
        crate::semantics::shared::get_type_skeleton(db, network_def).clone(),
    ))
}

/// Rightmost occurrence of "word" in "raw" on identifier boundaries,
/// as a byte span offset by "base".
/// example: "**.host.bla" with word "host" → span of "host" from base offset
fn find_word_span(raw: &str, base: usize, word: &str) -> Option<hir::Span> {
    let idx = raw.rfind(word)?;
    let bytes = raw.as_bytes();

    let before_ok = idx == 0 || (!bytes[idx - 1].is_ascii_alphanumeric() && bytes[idx - 1] != b'_');
    let end = idx + word.len();
    let after_ok = end == raw.len() || (!bytes[end].is_ascii_alphanumeric() && bytes[end] != b'_');

    (before_ok && after_ok).then(|| hir::Span::new(base + idx, base + end))
}

fn has_ancestor_kind(node: &tree_sitter::Node, kind: &str) -> bool {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == kind {
            return true;
        }
        current = n.parent();
    }
    false
}

/// Span of the name that declares the target not the hole span of the item
fn declaration_name_location(
    db: &dyn Db,
    target: &ResolvedDefinition<'_>,
) -> Option<(Url, hir::Span)> {
    match target {
        ResolvedDefinition::Module(def) => {
            let uri = def.uri(db);
            let file = db.get_source_file_input(uri)?;
            let ast = db.get_ast_for_file(&file)?;
            let item_span = def.span(db);

            let mut node = ast
                .root_node()
                .descendant_for_byte_range(item_span.start, item_span.start)?;
            // is_named filters out the keyword token, whose kind string is
            // identical to the item node kind (e.g. "simple")
            while !(node.is_named()
                && [
                    "module",
                    "simple",
                    "network",
                    "moduleinterface",
                    "channel",
                    "channel_interface",
                ]
                .contains(&node.kind()))
            {
                node = node.parent()?;
            }
            let name = node.child_by_field_name("name")?;
            Some((
                uri.clone(),
                hir::Span::new(name.start_byte(), name.end_byte()),
            ))
        }
        // for members the hir span already covers exactly the declaration
        ResolvedDefinition::Member { origin, span } => Some((origin.uri(db).clone(), *span)),
    }
}

// test the rename and reference feature
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{self, pos_of};

    fn setup(text: &str) -> (crate::db::RootLspDatabase, Url) {
        let (db, ned_file) = test_utils::setup_ned(text);
        let uri = ned_file.uri(&db).clone();
        (db, uri)
    }

    const TEXT: &str = "\
simple Car
{
    parameters:
        bool waveEnable;
}

simple RSU
{
}

module Scenario
{
    submodules:
        c1: Car;
        c2: Car;
}
";

    #[test]
    fn test_references_for_submodule_type() {
        let (db, uri) = setup(TEXT);

        // cursor on the type of c1 → declaration + two usages
        let pos = pos_of(TEXT, "c1: ", "Car");
        let refs = find_references(&db, &uri, pos, true).expect("should resolve");
        assert_eq!(refs.len(), 3, "refs: {:?}", refs);

        let refs = find_references(&db, &uri, pos, false).expect("should resolve");
        assert_eq!(refs.len(), 2, "refs: {:?}", refs);
    }

    #[test]
    fn test_references_from_declaration() {
        let (db, uri) = setup(TEXT);

        // cursor on the declaration name itself must find the same set
        let pos = pos_of(TEXT, "simple ", "Car");
        let refs = find_references(&db, &uri, pos, true).expect("should resolve");
        assert_eq!(refs.len(), 3, "refs: {:?}", refs);
    }

    #[test]
    fn test_rename_module_type() {
        let (db, uri) = setup(TEXT);

        let pos = pos_of(TEXT, "c1: ", "Car");
        let edit = rename(&db, &uri, pos, "Bike")
            .expect("rename should succeed")
            .expect("should produce edits");

        let changes = edit.changes.unwrap();
        let edits = changes.get(&uri).expect("edits in the test file");
        assert_eq!(edits.len(), 3, "edits: {:?}", edits);
        assert!(edits.iter().all(|e| e.new_text == "Bike"));
    }

    fn setup_workspace(ned_text: &str, ini_text: &str) -> (crate::db::RootLspDatabase, Url, Url) {
        let (db, ned_file, ini_file) = test_utils::setup_workspace(ned_text, ini_text);
        let ned_uri = ned_file.uri(&db).clone();
        let ini_uri = ini_file.uri(&db).clone();
        (db, ned_uri, ini_uri)
    }

    const INI_TEXT: &str = "\
[General]
network = Scenario
**.c1.typename = \"Car\"
**.c1.waveEnable = false
";

    #[test]
    fn test_rename_type_updates_ini_typename() {
        let (db, ned_uri, ini_uri) = setup_workspace(TEXT, INI_TEXT);

        let pos = pos_of(TEXT, "c1: ", "Car");
        let edit = rename(&db, &ned_uri, pos, "Bike")
            .expect("rename should succeed!!!!")
            .expect("should produce edits");

        let changes = edit.changes.unwrap();
        // 3 identifier occurrences in the ned file
        assert_eq!(changes.get(&ned_uri).map(|e| e.len()), Some(3));

        // 1 typename value in the ini file
        let ini_edits = changes.get(&ini_uri).expect("ini edits");
        assert_eq!(ini_edits.len(), 1, "ini edits: {:?}", ini_edits);

        assert_eq!(ini_edits[0].range.start.line, 2);
    }

    #[test]
    fn test_rename_network_updates_ini_value() {
        let (db, ned_uri, ini_uri) = setup_workspace(TEXT, INI_TEXT);

        let pos = pos_of(TEXT, "module ", "Scenario");
        let edit = rename(&db, &ned_uri, pos, "ScnearioManhattan")
            .expect("rename should succeed")
            .expect("should produce edits");

        let changes = edit.changes.unwrap();
        // declaration in the ned file + network value in the ini file
        assert_eq!(changes.get(&ned_uri).map(|e| e.len()), Some(1));
        let ini_edits = changes.get(&ini_uri).expect("ini edits");
        assert_eq!(ini_edits.len(), 1, "ini edits: {:?}", ini_edits);
        assert_eq!(ini_edits[0].range.start.line, 1);
    }

    #[test]
    fn test_rename_parameter_updates_ini_key() {
        let (db, ned_uri, ini_uri) = setup_workspace(TEXT, INI_TEXT);

        let pos = pos_of(TEXT, "bool ", "waveEnable");
        let edit = rename(&db, &ned_uri, pos, "waveEnabled")
            .expect("rename should succeed!!!!")
            .expect("should produce edits");

        let changes = edit.changes.unwrap();
        // parameter declaration in the ned file
        assert_eq!(changes.get(&ned_uri).map(|e| e.len()), Some(1));
        // key segment in the ini file
        let ini_edits = changes.get(&ini_uri).expect("ini edits");
        assert_eq!(ini_edits.len(), 1, "ini edits: {:?}", ini_edits);
        assert_eq!(ini_edits[0].range.start.line, 3);
        assert_eq!(ini_edits[0].new_text, "waveEnabled");
    }

    #[test]
    fn test_rename_submodule_updates_ini_key_segments() {
        let (db, ned_uri, ini_uri) = setup_workspace(TEXT, INI_TEXT);

        // cursor on the submodule name in "r1: Router"
        let pos = pos_of(TEXT, "submodules:", "c1");
        let edit = rename(&db, &ned_uri, pos, "car1")
            .expect("rename should succeed")
            .expect("should produce edits");

        let changes = edit.changes.unwrap();
        // declaration in the ned file
        assert_eq!(changes.get(&ned_uri).map(|e| e.len()), Some(1));
        // the r1 segment in both **.r1.typename and **.r1.addr
        let ini_edits = changes.get(&ini_uri).expect("ini edits");
        assert_eq!(ini_edits.len(), 2, "ini edits: {:?}", ini_edits);
        assert!(ini_edits.iter().all(|e| e.new_text == "car1"));
        assert!(
            ini_edits
                .iter()
                .all(|e| e.range.start.character == 3 && e.range.end.character == 5),
            "segment ranges: {:?}",
            ini_edits
        );
    }

    #[test]
    fn test_rename_type_in_packaged_file_updates_ini_typename() {
        const PKG_TEXT: &str = "\
package p;

simple Car
{
    parameters:
        bool waveEnable;
}

module Scenario
{
    submodules:
        c1: Car;
        c2: Car;
}
";
        let (db, ned_uri, ini_uri) = setup_workspace(PKG_TEXT, INI_TEXT);

        let pos = pos_of(PKG_TEXT, "c1: ", "Car");
        let edit = rename(&db, &ned_uri, pos, "Bike")
            .expect("rename should succeed")
            .expect("should produce edits");

        let changes = edit.changes.unwrap();
        assert_eq!(changes.get(&ned_uri).map(|e| e.len()), Some(3));
        let ini_edits = changes.get(&ini_uri).expect("ini typename edit");
        assert_eq!(ini_edits.len(), 1, "ini edits: {:?}", ini_edits);
    }

    #[test]
    fn test_rename_rejects_invalid_and_colliding_names() {
        let (db, uri) = setup(TEXT);
        let pos = pos_of(TEXT, "c1: ", "Car");

        // keyword
        assert!(rename(&db, &uri, pos, "module").is_err());
        // invalid syntax
        assert!(rename(&db, &uri, pos, "1abc").is_err());
        // collision with sibling type in the same package
        assert!(rename(&db, &uri, pos, "RSU").is_err());
    }

    #[test]
    fn test_rename_rejects_builtin_type_names() {
        let (mut db, uri) = setup(TEXT);

        // register a virtual std lib file like index_in_memory_file does
        let std_text = "simple IdealChannel\n{\n}\n";
        test_utils::add_ned_file(&mut db, "ned-internal:///std.ned", std_text);

        let pos = pos_of(TEXT, "c1: ", "Car");
        let result = rename(&db, &uri, pos, "IdealChannel");
        assert!(result.is_err(), "expected built-in rejection: {:?}", result);
        assert!(result.unwrap_err().contains("built-in"));
    }
}

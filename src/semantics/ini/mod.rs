pub mod builtin_config;
mod prelude;
pub mod shared;
mod validate_assignment;
pub mod validate_buildin_assignment;
pub mod validate_gate_assignment;
pub mod validate_network;

use validate_buildin_assignment::{
    check_builtin_value_type, check_seed_rng_index, validate_builtin_assignment,
};

use crate::{
    hir::{self},
    semantics::ned::shared::match_glob,
};
pub use prelude::*;

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
pub use validate_assignment::{process_typename_assignment, validate_ini_assignment};

pub use validate_gate_assignment::validate_gate_channel_assignment;

pub struct ParamOverride {
    pub clean_pattern: String,
    pub value: String,
}

pub struct ParsedIniSegment {
    pub name: String,
    pub index: Option<String>,
    pub gate_direction: Option<crate::hir::ned::gate_hir::GateDirection>,
}

// this struct holds the state of the network as we process the INI file, including flattened nodes, dynamic nodes, and context overrides
pub struct NetworkState<'a, 'db> {
    pub name: String,
    pub flattened_nodes: &'a [SkeletonNode<'db>],
    pub flattened_paths: Vec<String>,
    pub dynamic_nodes: Vec<(String, SkeletonNode<'db>)>, // dynamic nodes that are added to the network state as we process the ini file
    pub dynamic_contexts: HashMap<String, Vec<ContextOverride<'db>>>, // dynamic contexts that are added to the network state as we process the ini file
    pub param_overrides: HashMap<String, Vec<ParamOverride>>,
    context_cache: std::cell::RefCell<
        HashMap<Option<crate::db::DefIndex<'db>>, std::sync::Arc<crate::context::ItemContext<'db>>>,
    >, // bettern than Arc in this situation
    exact_index: HashMap<String, Vec<usize>>,
    suffix_index: HashMap<String, Vec<usize>>,
}

impl<'a, 'db> NetworkState<'a, 'db> {
    pub fn new(name: String, flattened_nodes: &'a [SkeletonNode<'db>]) -> Self {
        // A flattened path is the full path to a node (including the network name)
        // I can then use the assignment in the ini files to match against these flattened paths to
        // find the correct node to assign a value to.
        let flattened_paths: Vec<String> = flattened_nodes
            .iter()
            .map(|node| {
                if node.relative_path.is_empty() {
                    name.clone()
                } else {
                    format!("{}.{}", name, node.relative_path)
                }
            })
            .collect();

        let mut exact_index: HashMap<String, Vec<usize>> = HashMap::default();
        let mut suffix_index: HashMap<String, Vec<usize>> = HashMap::default();

        // this builds two indices → helps for fast lookups
        for (i, path) in flattened_paths.iter().enumerate() {
            exact_index.entry(path.clone()).or_default().push(i);

            for (pos, &b) in path.as_bytes().iter().enumerate() {
                if b == b'.' {
                    let suffix = &path[pos + 1..];
                    suffix_index.entry(suffix.to_string()).or_default().push(i);
                }
            }
            suffix_index.entry(path.clone()).or_default().push(i);
        }

        Self {
            name,
            flattened_nodes,
            flattened_paths,
            dynamic_nodes: Vec::new(),
            dynamic_contexts: HashMap::default(),
            param_overrides: HashMap::default(),
            context_cache: std::cell::RefCell::new(HashMap::default()),
            exact_index,
            suffix_index,
        }
    }

    pub fn get_cached_context(
        &self,
        db: &'db dyn crate::db::Db,
        node: &SkeletonNode<'db>,
    ) -> std::sync::Arc<crate::context::ItemContext<'db>> {
        if let Some(cached) = self.context_cache.borrow().get(&node.type_def) {
            return cached.clone();
        }
        // if the context is not cached → need to pay the cost of resolving (i think when I make it
        // a tracked function, this will actually reduce the performance → to many dependencies)
        let ctx = resolve_skeleton_context(db, node);
        self.context_cache
            .borrow_mut()
            .insert(node.type_def, ctx.clone());
        ctx
    }

    /// Finds the indices of flattened paths that match a given pattern, which may include wildcards.
    pub fn find_flattened_matches(&self, pattern: &str) -> Vec<usize> {
        let has_wildcards = pattern.contains('*') || pattern.contains('?');

        // no wildcard → cheap
        if !has_wildcards {
            return self.exact_index.get(pattern).cloned().unwrap_or_default();
        }

        // only the network name is a wildcard → cheap
        if let Some(suffix) = pattern.strip_prefix("**.")
            && !suffix.contains('*')
            && !suffix.contains('?')
        {
            return self.suffix_index.get(suffix).cloned().unwrap_or_default();
        }

        // wildcard → need to check all flattened paths (slower, but i found no better solution)
        self.flattened_paths
            .iter()
            .enumerate()
            .filter(|(_, path)| match_glob(pattern, path))
            .map(|(i, _)| i)
            .collect()
    }
}

/// Perform semantic analysis on an INI file, returning a list of diagnostics.
#[salsa::tracked]
pub fn semantic_analysis(
    db: &dyn crate::db::Db,
    ini_file: hir::SourceFile,
) -> Vec<hir::Diagnostic> {
    let ini_hir = crate::hir::ini::lower_to_hir(db, ini_file);

    let mut diagnostics = Vec::new();
    for section in ini_hir.sections(db) {
        diagnostics.extend(validate_section(db, ini_file, *section).iter().cloned());
    }
    diagnostics
}

/// Validate a single section of an INI file, returning a list of diagnostics.
#[salsa::tracked]
fn validate_section<'db>(
    db: &'db dyn crate::db::Db,
    ini_file: hir::SourceFile,
    section: crate::hir::ini::section_hir::IniSectionHir<'db>,
) -> Vec<hir::Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut ctx = ValidationCtx::new(db, &mut diagnostics, *ini_file.version(db));

    check_duplicate_keys(db, &mut ctx, section);

    if let Some((network_word, network_span)) =
        validate_network::resolve_network_word(db, ini_file, section)
    {
        let section_owns_network = section
            .configs(db)
            .iter()
            .any(|kv| kv.key.text(db) == "network");

        // check if it is a valid network definition
        if let Some(network_def) = validate_network::validate_network_def(
            db,
            &mut ctx,
            ini_file,
            network_word,
            network_span,
            section_owns_network, // avoid duplicate diagnostics
        ) {
            // get flattened network skeleton (cheap to compare → because it has no context that is
            // resolved)
            let skeleton = get_type_skeleton(db, network_def);

            let network_name = network_word.text(db);
            let mut network_state = NetworkState::new(network_name.clone(), &skeleton.nodes);

            // inheritance chain → General, BaseNetwork, BaseNetwork2, ...
            let raw_sec_name = section.name(db).text(db);
            let current_section_name = raw_sec_name
                .strip_prefix("Config ")
                .unwrap_or(raw_sec_name)
                .trim()
                .to_string();

            let section_chain = resolve_section_chain(db, ini_file, current_section_name.clone()); // tracked

            let chain_names: HashSet<String> = section_chain
                .iter()
                .map(|s| {
                    let raw = s.name(db).text(db);
                    raw.strip_prefix("Config ")
                        .unwrap_or(raw)
                        .trim()
                        .to_string()
                })
                .collect();

            for config in section.configs(db) {
                if config.key.text(db) != "extends" {
                    continue;
                }
                let raw_val = config.value.text(db);

                let mut offset = 0;
                for part in raw_val.split(',') {
                    let name =
                        part.trim_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'');
                    if !name.is_empty() {
                        let name_offset = part.find(name).unwrap_or(0);
                        let start = config.value_span.start + offset + name_offset;
                        if !chain_names.contains(name) {
                            ctx.error(
                                start,
                                start + name.len(),
                                format!(
                                    "Unknown section '{name}': no [Config {name}] found in this file."
                                ),
                            );
                        } else if name == current_section_name
                            || extends_reaches(
                                db,
                                ini_file,
                                name,
                                &current_section_name,
                                &mut HashSet::default(),
                            )
                        {
                            ctx.error(
                                start,
                                start + name.len(),
                                format!(
                                    "Circular extends: '{name}' leads back to '{current_section_name}'."
                                ),
                            );
                        }
                    }
                    offset += part.len() + 1; // +1 for the skipped comma
                }
            }

            // This is something I learned from compiler construction → multiple passes are often necessary to resolve all dependencies and ensure correctness.
            // I am not sure if this is the best approach, but it works reasonably well for now.

            // pass 1 → Typename Assignments
            for chain_sec in section_chain {
                let is_active = chain_sec.name(db).text(db) == section.name(db).text(db);
                for assignment in chain_sec.assignments(db) {
                    let raw_key = assignment.key.text(db);
                    let (segments, target_param_name) = parse_ini_key(raw_key);

                    if target_param_name == "typename" {
                        process_typename_assignment(
                            db,
                            &mut ctx,
                            &mut network_state,
                            assignment,
                            &segments,
                            is_active,
                        );
                    }
                }
            }

            // pass 2 → variable overrides
            // This is important to be able to assign a type of a like interface via the ini file
            for chain_sec in section_chain {
                for assignment in chain_sec.assignments(db) {
                    let raw_key = assignment.key.text(db);
                    let raw_value = assignment.value.text(db);

                    // split segments from target param
                    let (segments, target_param_name) = parse_ini_key(raw_key);

                    if target_param_name != "typename" {
                        let clean_pattern = segments
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join(".");

                        // store the override in the network state
                        network_state
                            .param_overrides
                            .entry(target_param_name.clone())
                            .or_default()
                            .push(ParamOverride {
                                clean_pattern,
                                value: raw_value.trim().to_string(),
                            });
                    }
                }
            }

            // pass 3 → Resolve parametric types from string overrides
            resolve_like_params(db, &mut ctx, &mut network_state);

            // pass 4: Validation for current section only

            // only accept known keywords TODO: not sure if this is a smart decision, since this
            // requires a constant update of the built-in config list.
            for config in section.configs(db) {
                let key = config.key.text(db);

                // immediately returns if not a seed or rng key (but I could optimize the structure)
                check_seed_rng_index(&mut ctx, section_chain, key, &config.key_span);

                if let Some(opt) = builtin_config::lookup_global(key)
                    .or_else(|| builtin_config::lookup_per_object(key))
                {
                    // check if the value matches the expected type
                    check_builtin_value_type(
                        &mut ctx,
                        opt,
                        config.value.text(db),
                        &config.value_span,
                    );
                }
            }

            for assignment in section.assignments(db) {
                let raw_key = assignment.key.text(db);
                let (segments, target_param_name) = parse_ini_key(raw_key);

                // skip
                if target_param_name == "typename" {
                    continue;
                }

                // only build in types are allowed to contain a dash
                if target_param_name.contains('-') {
                    // check if it is a build-in type
                    validate_builtin_assignment(
                        &mut ctx,
                        section_chain,
                        assignment,
                        &segments,
                        &target_param_name,
                    );
                    continue;
                }

                validate_ini_assignment(
                    &mut ctx,
                    &network_state,
                    assignment,
                    &segments,
                    &target_param_name,
                );
            }
        }
    } else {
        ctx.warning(
            section.header_span(db).start,
            section.header_span(db).end,
            format!(
                "No network configured for '{}' (or its base sections) - validation skipped.",
                section.name(db).text(db)
            ),
        );
    }

    diagnostics
}

/// Reports duplicated keys within a single section.
///
/// Opp resolves entries top-down and the first match wins → so I am flagging an override as an
/// error.
/// Duplicates across the extends chain are fine,
/// therefore only exact repetitions inside the same section are flagged.
fn check_duplicate_keys<'db>(
    db: &'db dyn crate::db::Db,
    ctx: &mut ValidationCtx,
    section: crate::hir::ini::section_hir::IniSectionHir<'db>,
) {
    let configs = section.configs(db);
    let assignments = section.assignments(db);

    let mut seen: HashSet<&str> = HashSet::default();
    for entry in configs.iter().chain(assignments.iter()) {
        let key = entry.key.text(db).as_str();
        if !seen.insert(key) {
            ctx.error(
                entry.key_span.start,
                entry.key_span.end,
                format!(
                    "Duplicate key '{key}': an earlier entry in this section already sets it, so this one has no effect."
                ),
            );
        }
    }
}

/// Parses an INI key into segments and the target parameter name.
pub fn parse_ini_key(key: &str) -> (Vec<ParsedIniSegment>, String) {
    let parts = split_ini_key(key);
    if parts.is_empty() {
        return (Vec::new(), String::new());
    }

    let target_param = parts.last().unwrap().to_string();
    let mut segments = Vec::new();

    for part in &parts[..parts.len() - 1] {
        if *part == "**" {
            segments.push(ParsedIniSegment {
                name: "**".to_string(),
                index: None,
                gate_direction: None,
            });
            continue;
        }
        // check if the part contains an index → submodule[0] or gate$o[0]
        #[allow(clippy::collapsible_if)]
        if let Some(start_idx) = part.find('[') {
            if let Some(end_idx) = part.find(']') {
                let (name, gate_direction) = parse_gate_suffix(&part[..start_idx]);
                segments.push(ParsedIniSegment {
                    name,
                    index: Some(part[start_idx + 1..end_idx].to_string()),
                    gate_direction,
                });
                continue;
            }
        }
        let (name, gate_direction) = parse_gate_suffix(part);
        segments.push(ParsedIniSegment {
            name,
            index: None,
            gate_direction,
        });
    }
    (segments, target_param)
}

/// Resolves like parameters in the network state by checking for overrides and applying them to the flattened nodes.
pub fn resolve_like_params<'db>(
    db: &'db dyn crate::db::Db,
    _ctx: &mut ValidationCtx<'_, 'db>,
    network_state: &mut NetworkState<'_, 'db>,
) {
    let mut typename_assignments: Vec<(String, String, String)> = Vec::new();

    for (idx, node) in network_state.flattened_nodes.iter().enumerate() {
        let Some(ref param_name) = node.like_param else {
            continue;
        };

        let Some(overrides) = network_state.param_overrides.get(param_name) else {
            continue;
        };

        let absolute_path = &network_state.flattened_paths[idx];

        // The like_param is on the parent → strip the last segment to get parent path
        let parent_path = match absolute_path.rfind('.') {
            Some(pos) => &absolute_path[..pos],
            None => absolute_path.as_str(),
        };

        for ov in overrides {
            if match_glob(&ov.clean_pattern, parent_path) {
                let trimmed = ov.value.trim();
                if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() > 2 {
                    typename_assignments.push((
                        absolute_path.clone(),
                        node.relative_path.clone(),
                        trimmed[1..trimmed.len() - 1].to_string(),
                    ));
                    break;
                }
            }
        }
    }

    // Now we have a list of typename assignments → we need to resolve them and update the network state
    // This allows to set a typename and then correctly validate if the parameter assignments are
    // valid for the type.
    for (absolute_path, relative_path, type_name) in typename_assignments {
        let name_word = crate::hir::Word::new(db, type_name.clone());
        let def_opt = crate::db::package_tree::resolve_global_type(db, name_word);

        if let Some(def_index) = def_opt {
            if let Some(res_hir) = crate::context::resolve_item_hir(db, *def_index) {
                let new_ctx =
                    crate::context::get_item_context(db, res_hir.item, res_hir.definitions.clone());

                network_state
                    .dynamic_contexts
                    .entry(absolute_path.clone())
                    .or_default()
                    .push(ContextOverride {
                        source_pattern: absolute_path.clone(),
                        context: new_ctx.clone(),
                    });
            }

            let new_skeleton = get_type_skeleton(db, *def_index);

            for skel_node in new_skeleton.nodes.iter() {
                if skel_node.relative_path.is_empty() {
                    continue;
                }

                let new_path = if relative_path.is_empty() {
                    skel_node.relative_path.clone()
                } else {
                    format!("{}.{}", relative_path, skel_node.relative_path)
                };

                let dynamic_abs_path = format!("{}.{}", network_state.name, new_path);

                network_state.dynamic_nodes.push((
                    dynamic_abs_path,
                    SkeletonNode {
                        relative_path: new_path,
                        is_vector: skel_node.is_vector,
                        vector_size: skel_node.vector_size.clone(),
                        type_def: skel_node.type_def,
                        like_param: skel_node.like_param.clone(),
                    },
                ));
            }
        }
    }
}

/// Populates a NetworkState from the section chain of a given section.
pub fn populate_network_state<'db>(
    db: &'db dyn crate::db::Db,
    ini_file: hir::SourceFile,
    section: crate::hir::ini::section_hir::IniSectionHir<'db>,
    state: &mut NetworkState<'_, 'db>,
) {
    let raw_sec_name = section.name(db).text(db);
    let current_section_name = raw_sec_name
        .strip_prefix("Config ")
        .unwrap_or(raw_sec_name)
        .trim()
        .to_string();
    let section_chain = resolve_section_chain(db, ini_file, current_section_name);

    let mut dummy_diagnostics = Vec::new();
    let mut ctx = crate::semantics::shared::ValidationCtx::new(db, &mut dummy_diagnostics, 0);

    // first resolve typename assignments
    for chain_sec in section_chain {
        for assignment in chain_sec.assignments(db) {
            let raw_key = assignment.key.text(db);
            let (segments, target_param_name) = parse_ini_key(raw_key);
            if target_param_name == "typename" {
                process_typename_assignment(db, &mut ctx, state, assignment, &segments, false);
            }
        }
    }

    // resolve variable overrides
    for chain_sec in section_chain {
        for assignment in chain_sec.assignments(db) {
            let raw_key = assignment.key.text(db);
            let raw_value = assignment.value.text(db);
            let (segments, target_param_name) = parse_ini_key(raw_key);
            if target_param_name != "typename" {
                let clean_pattern = segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                state
                    .param_overrides
                    .entry(target_param_name.clone())
                    .or_default()
                    .push(ParamOverride {
                        clean_pattern,
                        value: raw_value.trim().to_string(),
                    });
            }
        }
    }

    // resolve like parameters based on the overrides
    resolve_like_params(db, &mut ctx, state);
}

/// Resolves a section by name, returning the corresponding IniSectionHir if it exists.
#[salsa::tracked]
pub fn resolve_section<'db>(
    db: &'db dyn crate::db::Db,
    ini_file: hir::SourceFile,
    name: String,
) -> Option<crate::hir::ini::section_hir::IniSectionHir<'db>> {
    get_section_map(db, ini_file).get(&name).copied()
}

#[salsa::tracked]
pub fn resolve_section_chain<'db>(
    db: &'db dyn crate::db::Db,
    ini_file: hir::SourceFile,
    target_section_name: String,
) -> Vec<crate::hir::ini::section_hir::IniSectionHir<'db>> {
    let mut chain = Vec::new();
    let mut visited = HashSet::default();

    build_chain(db, ini_file, &target_section_name, &mut chain, &mut visited);
    chain
}

// recursive builds the chain of sections, following extends and General
fn build_chain<'db>(
    db: &'db dyn crate::db::Db,
    ini_file: hir::SourceFile,
    section_name: &str,
    chain: &mut Vec<crate::hir::ini::section_hir::IniSectionHir<'db>>,
    visited: &mut HashSet<String>,
) {
    // avoid infinite loops
    // TODO: show error → passing in the ctx to this function is a bit tricky
    if !visited.insert(section_name.to_string()) {
        return;
    }

    if let Some(section) = resolve_section(db, ini_file, section_name.to_string()) {
        let extended_sections = extract_extends_list(db, *section);

        // add parents first
        if !extended_sections.is_empty() {
            for parent_name in extended_sections {
                build_chain(db, ini_file, parent_name, chain, visited);
            }
        } else if section_name != "General"
            && resolve_section(db, ini_file, "General".to_string()).is_some()
        {
            // if nothing is defined, the section implicitly extends General
            build_chain(db, ini_file, "General", chain, visited);
        }

        // push current section last
        chain.push(*section);
    }
}

/// Extracts the list of parent sections from the "extends" key in a sections configs.
#[salsa::tracked]
fn extract_extends_list<'db>(
    db: &'db dyn crate::db::Db,
    section: crate::hir::ini::section_hir::IniSectionHir<'db>,
) -> Vec<String> {
    let mut parents = Vec::new();

    for config in section.configs(db) {
        if config.key.text(db) == "extends" {
            let raw_val = config.value.text(db);
            // Remove quotes if they exist, then split by comma
            let clean_val = raw_val.trim_matches(|c| c == '"' || c == '\'');

            for part in clean_val.split(',') {
                let trimmed = part.trim();
                if !trimmed.is_empty() {
                    parents.push(trimmed.to_string());
                }
            }
        }
    }
    parents
}

// important for circular dependency detection
pub(crate) fn extends_reaches(
    db: &dyn crate::db::Db,
    ini_file: hir::SourceFile,
    from: &str,
    target: &str,
    visited: &mut HashSet<String>,
) -> bool {
    if !visited.insert(from.to_string()) {
        return false; // this branch was already explored
    }
    let Some(section) = resolve_section(db, ini_file, from.to_string()) else {
        return false;
    };
    extract_extends_list(db, *section)
        .iter()
        .any(|parent| parent == target || extends_reaches(db, ini_file, parent, target, visited))
}

/// Build a map of section names to IniSectionHir for quick lookup.
#[salsa::tracked]
pub fn get_section_map<'db>(
    db: &'db dyn crate::db::Db,
    ini_file: hir::SourceFile,
) -> HashMap<String, crate::hir::ini::section_hir::IniSectionHir<'db>> {
    let mut map = HashMap::default();
    let ini_hir = crate::hir::ini::lower_to_hir(db, ini_file);

    for section in ini_hir.sections(db) {
        let raw_name = section.name(db).text(db);
        let clean_name = raw_name
            .strip_prefix("Config ")
            .unwrap_or(raw_name)
            .trim()
            .to_string();
        map.insert(clean_name, *section);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{setup_ini, setup_workspace as setup_workspace_files};

    fn duplicate_key_diags(text: &str) -> Vec<hir::Diagnostic> {
        let (db, ini_file) = setup_ini(text);
        semantic_analysis(&db, ini_file)
            .iter()
            .filter(|d| d.message.contains("Duplicate key"))
            .cloned()
            .collect()
    }

    #[test]
    fn test_duplicate_assignment_key_in_same_section() {
        let text = "\
[General]
**.host.port = 80
**.host.port = 8080
";
        let diags = duplicate_key_diags(text);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, crate::hir::Severity::Error);
        // the second (shadowed) occurrence is flagged
        assert_eq!(diags[0].start, text.rfind("**.host.port").unwrap());
    }

    #[test]
    fn test_duplicate_config_key_in_same_section() {
        let text = "\
[General]
network = AutoTSN
network = FramePreemptionBase
";
        let diags = duplicate_key_diags(text);
        assert_eq!(diags.len(), 1);
    }

    /// Registers a ned file alongside the ini file so network resolution works.
    fn setup_workspace(
        ned_text: &str,
        ini_text: &str,
    ) -> (crate::db::RootLspDatabase, hir::SourceFile) {
        let (db, _ned_file, ini_file) = setup_workspace_files(ned_text, ini_text);
        (db, ini_file)
    }

    const VECTOR_NED: &str = "\
simple R
{
    parameters:
        int bla;
}

module W
{
    submodules:
        radio: R;
}

network Net
{
    parameters:
        int n = default(0);
    submodules:
        wlan[n]: W;
}
";

    #[test]
    fn test_intermediate_index_out_of_bounds() {
        let ini = "\
[General]
network = Net
Net.n = 2
**.wlan[5].radio.bla = 1
";
        let (db, ini_file) = setup_workspace(VECTOR_NED, ini);
        let diags = semantic_analysis(&db, ini_file);
        let messages: Vec<&String> = diags.iter().map(|d| &d.message).collect();
        assert!(
            messages.iter().any(|m| m.contains("Index out of bounds")),
            "expected out-of-bounds error, got: {:?}",
            messages
        );
    }

    #[test]
    fn test_intermediate_index_in_bounds() {
        let ini = "\
[General]
network = Net
Net.n = 2
**.wlan[1].radio.bla = 1
";
        let (db, ini_file) = setup_workspace(VECTOR_NED, ini);
        let diags = semantic_analysis(&db, ini_file);
        let messages: Vec<&String> = diags.iter().map(|d| &d.message).collect();
        assert!(
            !messages.iter().any(|m| m.contains("Index out of bounds")),
            "unexpected out-of-bounds error: {:?}",
            messages
        );
    }

    #[test]
    fn test_index_pattern_matches() {
        // pattern matchin on indexes → [*0] on size 25 matches indices 0, 10, 20
        let ini = "\
[General]
network = Net
Net.n = 25
**.wlan[*0].radio.bla = 1
";
        let (db, ini_file) = setup_workspace(VECTOR_NED, ini);
        let diags = semantic_analysis(&db, ini_file);
        let messages: Vec<&String> = diags.iter().map(|d| &d.message).collect();
        assert!(
            !messages
                .iter()
                .any(|m| m.contains("does not match any index")
                    || m.contains("Invalid index format")),
            "unexpected diagnostic for matching index pattern: {:?}",
            messages
        );
    }

    #[test]
    fn test_index_pattern_no_match() {
        // [*7] on size 5 matches nothing
        let ini = "\
[General]
network = Net
Net.n = 5
**.wlan[*7].radio.bla = 1
";
        let (db, ini_file) = setup_workspace(VECTOR_NED, ini);
        let diags = semantic_analysis(&db, ini_file);
        assert!(
            diags.iter().any(|d| {
                d.message.contains("does not match any index")
                    && d.severity == crate::hir::Severity::Warning
            }),
            "expected no-match warning, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_index_pattern_inner_wildcard() {
        // [8*8]
        let ini = "\
[General]
network = Net
Net.n = 100
**.wlan[8*8].radio.bla = 1
";
        let (db, ini_file) = setup_workspace(VECTOR_NED, ini);
        let diags = semantic_analysis(&db, ini_file);
        let messages: Vec<&String> = diags.iter().map(|d| &d.message).collect();
        assert!(
            !messages
                .iter()
                .any(|m| m.contains("does not match any index")
                    || m.contains("Invalid index format")),
            "unexpected diagnostic for [8*8] with size 100: {:?}",
            messages
        );
    }

    #[test]
    fn test_index_pattern_size_above_cap() {
        // size 20000 exceeds the scan cap → pattern is not scanned
        let ini = "\
[General]
network = Net
Net.n = 20000
**.wlan[*0].radio.bla = 1
";
        let (db, ini_file) = setup_workspace(VECTOR_NED, ini);
        let diags = semantic_analysis(&db, ini_file);
        assert!(
            diags.iter().any(|d| {
                d.message.contains("cannot be fully checked")
                    && d.severity == crate::hir::Severity::Warning
            }),
            "expected cannot be becked warning warning, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        // the scan must not have run → no false no-match warning
        assert!(
            !diags
                .iter()
                .any(|d| d.message.contains("does not match any index")),
            "scan should be skipped above the cap"
        );
    }

    #[test]
    fn test_same_key_in_different_sections_is_ok() {
        let text = "\
[General]
**.host.port = 80

[Config Alt]
**.host.port = 8080
";
        let diags = duplicate_key_diags(text);
        assert!(diags.is_empty());
    }
}

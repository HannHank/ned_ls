use std::sync::Arc;

use crate::hir::ned::import_hir::ImportPattern;
use crate::hir::ned::submodules_hir::SubmoduleDefinition;
use crate::hir::{ned::import_hir::ImportHir, tree_utils::extract_field};
use crate::semantics::ned::shared::ParentScope;
use crate::{db, hir, utils};
use tower_lsp::lsp_types::{Location, Range};

const BUILTIN_URI: &str = "ned-internal://std.ned";

#[allow(dead_code)]
pub enum GotoResult {
    ImportSuccess(String, usize, Vec<tower_lsp::lsp_types::LocationLink>),
    ImportNotFound(String),
    ImportNoMatch(String),
    NoWord,
    SymbolFound(String, Vec<tower_lsp::lsp_types::LocationLink>),
    SymbolNotFound(String),
}

/// Represents different kinds of definitions that can be resolved
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ResolvedDefinition<'db> {
    Module(db::DefIndex<'db>),
    Member {
        origin: db::DefIndex<'db>,
        span: hir::Span,
    },
}

/// Converts a ResolvedDefinition into a LocationLink for LSP
impl<'db> ResolvedDefinition<'db> {
    pub fn to_location_link(
        &self,
        db: &dyn crate::db::Db,
        origin_rope: &ropey::Rope,
        origin_span: hir::Span,
    ) -> Option<tower_lsp::lsp_types::LocationLink> {
        let (def_index, span) = match self {
            ResolvedDefinition::Module(index) => (*index, index.span(db)),
            ResolvedDefinition::Member { origin, span } => (*origin, *span),
        };

        let uri = def_index.uri(db);
        let rope = db.get_source_file_input(uri)?.rope(db);

        // Check if resolved to build-in type and return None → maybe in the future i will create a
        // temp file to allow navigating to the build in types
        if uri.as_str() == BUILTIN_URI {
            return None;
        }

        Some(tower_lsp::lsp_types::LocationLink {
            target_uri: uri.clone(),
            target_range: Range {
                start: utils::byte_to_lsp_position(rope, span.start)?,
                end: utils::byte_to_lsp_position(rope, span.end)?,
            },
            target_selection_range: Range {
                start: utils::byte_to_lsp_position(rope, span.start)?,
                end: utils::byte_to_lsp_position(rope, span.end)?,
            },
            origin_selection_range: Some(Range {
                start: utils::byte_to_lsp_position(origin_rope, origin_span.start)?,
                end: utils::byte_to_lsp_position(origin_rope, origin_span.end)?,
            }),
        })
    }

    pub fn to_location(&self, db: &dyn crate::db::Db) -> Option<Location> {
        let (def_index, span) = match self {
            ResolvedDefinition::Module(index) => (*index, index.span(db)),
            ResolvedDefinition::Member { origin, span } => (*origin, *span),
        };

        let uri = def_index.uri(db);
        let rope = db.get_source_file_input(uri)?.rope(db);

        if uri.as_str() == BUILTIN_URI {
            return None;
        }

        Some(Location {
            uri: uri.clone(),
            range: Range {
                start: utils::byte_to_lsp_position(rope, span.start)?,
                end: utils::byte_to_lsp_position(rope, span.end)?,
            },
        })
    }
}

/// A scope resolver that understands nested contexts
pub struct ScopeResolver<'db> {
    db: &'db dyn crate::db::Db,
    ned_file: hir::SourceFile,
    hir: hir::NedFileHir<'db>,
}

impl<'db> ScopeResolver<'db> {
    pub fn new(db: &'db dyn crate::db::Db, ned_file: hir::SourceFile) -> Self {
        let hir = hir::ned::lower_to_hir(db, ned_file);
        Self {
            db,
            ned_file,
            hir: *hir,
        }
    }

    // main entry point for resolving a symbol at a given node and byte offset
    pub fn resolve(
        &self,
        node: &tree_sitter::Node,
        byte_offset: usize,
        word: &str,
    ) -> Option<Vec<ResolvedDefinition<'db>>> {
        // Check Imports First TODO: would be nicer to use the kind of the node to avoid unnecessary
        // loops but for now this is fine
        if let Some(import_hir) = self.get_import_at_position(byte_offset) {
            return self.resolve_import_pattern(import_hir);
        }

        // Fetch the Base Scope
        let containing_module = self.find_containing_module(node.start_byte())?;
        let visible_defs = crate::db::visible_definitions(self.db, self.ned_file);
        let base_context =
            crate::context::get_item_context(self.db, *containing_module, visible_defs.clone());

        let uri = self.ned_file.uri(self.db);
        let rope = self.db.get_source_file_input(uri).unwrap().rope(self.db);

        // Adjust matching node to target name → I tried adding a name field to all nodes inside the
        // TS grammar which makes it necessary to search for the surrounding type
        let mut match_node = *node;
        if match_node.kind() == "name"
            && let Some(parent) = match_node.parent()
        {
            match_node = parent;
        }

        // Narrow context if we are inside a submodule block
        let active_context = self.determine_active_context(
            node,
            base_context,
            containing_module,
            rope,
            visible_defs.clone(),
        );

        // Call the corresponding node handlers
        match match_node.kind() {
            "parameter" => return self.resolve_parameter(word, &active_context),
            "type" => return self.resolve_type(word, &active_context),
            "module" => {
                if let Some(res) = self.resolve_module(word, &active_context) {
                    return Some(res);
                }
            }
            "string_literal" => return self.resolve_string_literal(word),
            "gate" => {
                if let Some(res) =
                    self.resolve_gate(word, &match_node, base_context, containing_module, rope)
                {
                    return Some(res);
                }
            }
            "pattern" | "pattern_elem" | "pattern_name" => {
                return self.resolve_pattern(
                    node,
                    byte_offset,
                    &active_context,
                    containing_module,
                    rope,
                );
            }
            _ => {}
        }

        log::debug!("checking local types");

        // direct active context fallback
        if let Some(res) = self.resolve_local_fallback(word, &active_context) {
            log::debug!("result: {:?}", res);
            return Some(res);
        }

        log::debug!("checking global goto_def");
        // global workspace & index fallback
        self.resolve_global_fallback(word)
    }

    /// Walks up the ancestor chain to determine the active context, adjusting for any submodule definitions encountered along the way
    pub fn determine_active_context(
        &self,
        node: &tree_sitter::Node,
        base_context: &Arc<crate::context::ItemContext<'db>>,
        containing_module: &hir::ItemHir<'db>,
        rope: &ropey::Rope,
        visible_defs: std::sync::Arc<Vec<crate::db::DefIndex<'db>>>,
    ) -> Arc<crate::context::ItemContext<'db>> {
        let mut active_context = base_context.clone();
        let mut current_ancestor = node.parent();

        let parent = ParentScope {
            params: containing_module.parameters(self.db),
            context: base_context,
        };

        while let Some(ancestor) = current_ancestor {
            if ancestor.kind() == "submodule" {
                let node_is_in_body = ancestor
                    .children(&mut ancestor.walk())
                    .find(|c| c.kind() == "{")
                    .map(|brace| node.start_byte() > brace.start_byte())
                    .unwrap_or(false);

                if !node_is_in_body {
                    break; // cursor is on the header → keep base_context
                }

                if let Some(name_node) = ancestor.child_by_field_name("name")
                    && let Some(extracted) = extract_field(self.db, &name_node, rope)
                {
                    let sub_name = extracted.word.text(self.db);
                    if let Some(sub_mod) = base_context.submodules.get(sub_name.as_str()) {
                        let mut actual_type_str =
                            crate::semantics::ned::shared::get_submodule_actual_type(
                                self.db,
                                &parent,
                                sub_name,
                                &sub_mod.item,
                            )
                            .map(|(t, _)| t)
                            .unwrap_or_default();

                        if actual_type_str.is_empty()
                            && let SubmoduleDefinition::Parametric {
                                like_expr: Some(expr),
                                ..
                            } = &sub_mod.item.definition
                            && let Some((concrete, _)) =
                                crate::semantics::ned::shared::extract_concrete_type(expr)
                        {
                            actual_type_str = concrete;
                        }

                        let clean_type_str =
                            actual_type_str.trim_matches(|c| c == '"' || c == '\'');

                        let next_ctx_opt =
                            if let Some(local_type) = base_context.types.get(clean_type_str) {
                                Some(
                                    crate::context::get_item_context(
                                        self.db,
                                        local_type.item,
                                        visible_defs.clone(),
                                    )
                                    .clone(),
                                )
                            } else {
                                crate::context::get_submodule_context_from_type(
                                    self.db,
                                    clean_type_str,
                                    visible_defs.clone(),
                                )
                            };

                        if let Some(next_ctx) = next_ctx_opt {
                            active_context = next_ctx;
                        }
                    }
                }
                break;
            } else if ancestor.kind() == "module"
                || ancestor.kind() == "simple_module"
                || ancestor.kind() == "network"
            {
                break;
            }
            current_ancestor = ancestor.parent();
        }

        active_context
    }

    // Resolve a pattern node, handling wildcards and nested contexts
    fn resolve_pattern(
        &self,
        node: &tree_sitter::Node,
        byte_offset: usize,
        active_context: &Arc<crate::context::ItemContext<'db>>,
        containing_module: &hir::ItemHir<'db>,
        rope: &ropey::Rope,
    ) -> Option<Vec<ResolvedDefinition<'db>>> {
        let mut pattern_node = *node;
        while pattern_node.kind() != "pattern" && pattern_node.parent().is_some() {
            pattern_node = pattern_node.parent().unwrap();
        }

        if pattern_node.kind() != "pattern" {
            return None;
        }

        let mut active_contexts = vec![active_context.clone()];
        let mut walk = pattern_node.walk();
        let visible_defs = crate::db::visible_definitions(self.db, self.ned_file);

        for path_elem in pattern_node.children_by_field_name("path", &mut walk) {
            let name_node = path_elem.child_by_field_name("name").unwrap_or(path_elem);
            let sub_name = extract_field(self.db, &name_node, rope)?.word.text(self.db);
            let is_cursor_here =
                byte_offset >= name_node.start_byte() && byte_offset <= name_node.end_byte();

            let mut next_level_contexts = Vec::new();
            let mut exact_click_results = Vec::new();

            for ctx in &active_contexts {
                let parent = ParentScope {
                    params: containing_module.parameters(self.db),
                    context: ctx,
                };
                if sub_name == "*" || sub_name == "**" {
                    for (actual_name, sub) in &ctx.submodules {
                        if is_cursor_here {
                            exact_click_results.push(ResolvedDefinition::Member {
                                origin: sub.origin,
                                span: sub.item.span,
                            });
                        }

                        let (actual_type_str, _) =
                            crate::semantics::ned::shared::get_submodule_actual_type(
                                self.db,
                                &parent,
                                actual_name,
                                &sub.item,
                            )
                            .unwrap_or_default();

                        let next_ctx_opt = if let Some(local_type) = ctx.types.get(&actual_type_str)
                        {
                            Some(
                                crate::context::get_item_context(
                                    self.db,
                                    local_type.item,
                                    visible_defs.clone(),
                                )
                                .clone(),
                            )
                        } else {
                            crate::context::get_submodule_context_from_type(
                                self.db,
                                &actual_type_str,
                                visible_defs.clone(),
                            )
                        };

                        if let Some(next_ctx) = next_ctx_opt {
                            next_level_contexts.push(next_ctx);
                        }
                    }
                } else if let Some(sub) = ctx.submodules.get(sub_name) {
                    if is_cursor_here {
                        exact_click_results.push(ResolvedDefinition::Member {
                            origin: sub.origin,
                            span: sub.item.span,
                        });
                    }

                    let (actual_type_str, _) =
                        crate::semantics::ned::shared::get_submodule_actual_type(
                            self.db, &parent, sub_name, &sub.item,
                        )
                        .unwrap_or_default();

                    let next_ctx_opt = if let Some(local_type) = ctx.types.get(&actual_type_str) {
                        Some(
                            crate::context::get_item_context(
                                self.db,
                                local_type.item,
                                visible_defs.clone(),
                            )
                            .clone(),
                        )
                    } else {
                        crate::context::get_submodule_context_from_type(
                            self.db,
                            &actual_type_str,
                            visible_defs.clone(),
                        )
                    };

                    if let Some(next_ctx) = next_ctx_opt {
                        next_level_contexts.push(next_ctx);
                    }
                }
            }

            if is_cursor_here && !exact_click_results.is_empty() {
                return Some(exact_click_results);
            }

            active_contexts = next_level_contexts;
            if active_contexts.is_empty() {
                return None;
            }
        }

        // Resolving the last part of the pattern
        if let Some(param_elem) = pattern_node.child_by_field_name("parameter") {
            let name_node = param_elem.child_by_field_name("name").unwrap_or(param_elem);

            if byte_offset >= name_node.start_byte() && byte_offset <= name_node.end_byte() {
                let target_param_name =
                    extract_field(self.db, &name_node, rope)?.word.text(self.db);
                let mut final_results = Vec::new();

                for ctx in &active_contexts {
                    if target_param_name == "*" || target_param_name == "**" {
                        for param in ctx.parameters.values() {
                            final_results.push(ResolvedDefinition::Member {
                                origin: param.origin,
                                span: param.item.span(self.db),
                            });
                        }
                        for gate in ctx.gates.values() {
                            final_results.push(ResolvedDefinition::Member {
                                origin: gate.origin,
                                span: gate.item.span,
                            });
                        }
                    } else if let Some(param) = ctx.parameters.get(target_param_name) {
                        final_results.push(ResolvedDefinition::Member {
                            origin: param.origin,
                            span: param.item.span(self.db),
                        });
                    } else if let Some(gate) = ctx.gates.get(target_param_name) {
                        final_results.push(ResolvedDefinition::Member {
                            origin: gate.origin,
                            span: gate.item.span,
                        });
                    }
                }

                if !final_results.is_empty() {
                    return Some(final_results);
                }
            }
        }
        None
    }

    fn resolve_gate(
        &self,
        word: &str,
        match_node: &tree_sitter::Node,
        context: &crate::context::ItemContext<'db>,
        containing_module: &hir::ItemHir<'db>,
        rope: &ropey::Rope,
    ) -> Option<Vec<ResolvedDefinition<'db>>> {
        let parent = match_node.parent()?;
        let module_node = parent.child_by_field_name("module")?;
        let module_word = extract_field(self.db, &module_node, rope)?;
        let sub_name = module_word.word.text(self.db);
        let sub_mod = context.submodules.get(sub_name)?;
        let visible_defs = crate::db::visible_definitions(self.db, self.ned_file);

        let mut candidates = Vec::new();

        let parent = ParentScope {
            params: containing_module.parameters(self.db),
            context,
        };

        if let Some((actual_type, _)) = crate::semantics::ned::shared::get_submodule_actual_type(
            self.db,
            &parent,
            sub_name,
            &sub_mod.item,
        ) && !actual_type.is_empty()
        {
            candidates.push(
                actual_type
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_string(),
            );
        }

        // use interface_name when actual type resolution fails
        if let SubmoduleDefinition::Parametric { interface_name, .. } = &sub_mod.item.definition {
            candidates.push(interface_name.clone());
        }

        for candidate in candidates {
            let next_ctx_opt = if let Some(local_type) = context.types.get(&candidate) {
                Some(
                    crate::context::get_item_context(
                        self.db,
                        local_type.item,
                        visible_defs.clone(),
                    )
                    .clone(),
                )
            } else {
                crate::context::get_submodule_context_from_type(
                    self.db,
                    &candidate,
                    visible_defs.clone(),
                )
            };

            if let Some(next_ctx) = next_ctx_opt
                && let Some(gate) = next_ctx.gates.get(word)
            {
                return Some(vec![ResolvedDefinition::Member {
                    origin: gate.origin,
                    span: gate.item.span,
                }]);
            }
        }

        None
    }

    fn resolve_parameter(
        &self,
        word: &str,
        active_context: &crate::context::ItemContext<'db>,
    ) -> Option<Vec<ResolvedDefinition<'db>>> {
        active_context.parameters.get(word).map(|param| {
            vec![ResolvedDefinition::Member {
                origin: param.origin,
                span: param.item.span(self.db),
            }]
        })
    }

    // If a submodule is not parametric it has a type
    fn resolve_type(
        &self,
        word: &str,
        active_context: &crate::context::ItemContext<'db>,
    ) -> Option<Vec<ResolvedDefinition<'db>>> {
        // type names cannot contain wildcards → do not let them match import patterns
        if word.contains('*') {
            return None;
        }

        // Local (inner) types shadow visible/global ones
        if let Some(local_type) = active_context.types.get(word) {
            return Some(vec![ResolvedDefinition::Member {
                origin: local_type.origin,
                span: local_type.item.span(self.db),
            }]);
        }

        // find_def handles both cases: fqns via the package tree and simple
        // names via the visible defs → same resolution the validator uses
        let visible_defs = crate::db::visible_definitions(self.db, self.ned_file);
        crate::db::find_def(self.db, word, visible_defs)
            .map(|def| vec![ResolvedDefinition::Module(def)])
    }

    fn resolve_module(
        &self,
        word: &str,
        active_context: &crate::context::ItemContext<'db>,
    ) -> Option<Vec<ResolvedDefinition<'db>>> {
        active_context.submodules.get(word).map(|sub| {
            vec![ResolvedDefinition::Member {
                origin: sub.origin,
                span: sub.item.span,
            }]
        })
    }

    fn resolve_string_literal(&self, word: &str) -> Option<Vec<ResolvedDefinition<'db>>> {
        let clean_word = word.trim_matches(|c| c == '"' || c == '\'');

        let visible_defs = crate::db::visible_definitions(self.db, self.ned_file);
        if let Some(def) = visible_defs
            .iter()
            .find(|d| d.simple_name(self.db).text(self.db) == clean_word)
        {
            return Some(vec![ResolvedDefinition::Module(*def)]);
        }

        let indexes = crate::db::package_tree::workspace_indexes(self.db);
        if let Some(defs) = indexes.suffix_index.get(clean_word)
            && let Some(def) = defs.first()
        {
            return Some(vec![ResolvedDefinition::Module(*def)]);
        }
        None
    }

    fn resolve_local_fallback(
        &self,
        word: &str,
        active_context: &crate::context::ItemContext<'db>,
    ) -> Option<Vec<ResolvedDefinition<'db>>> {
        if let Some(gate) = active_context.gates.get(word) {
            return Some(vec![ResolvedDefinition::Member {
                origin: gate.origin,
                span: gate.item.span,
            }]);
        }
        if let Some(sub) = active_context.submodules.get(word) {
            return Some(vec![ResolvedDefinition::Member {
                origin: sub.origin,
                span: sub.item.span,
            }]);
        }
        if let Some(param) = active_context.parameters.get(word) {
            return Some(vec![ResolvedDefinition::Member {
                origin: param.origin,
                span: param.item.span(self.db),
            }]);
        }
        None
    }

    // Last resort but should not be needed
    fn resolve_global_fallback(&self, word: &str) -> Option<Vec<ResolvedDefinition<'db>>> {
        // FQN import pattern fallback
        if word.contains('.') {
            let pattern = ImportPattern::new(self.db, word.to_string());
            if let Some(results) = crate::db::resolve_import(self.db, pattern)
                && let Some(def) = results.first()
            {
                return Some(vec![ResolvedDefinition::Module(*def)]);
            }
            return None;
        }

        // Global Visibility Fallback
        let visible_defs = crate::db::visible_definitions(self.db, self.ned_file);
        if let Some(def) = visible_defs
            .iter()
            .find(|d| d.simple_name(self.db).text(self.db) == word)
        {
            return Some(vec![ResolvedDefinition::Module(*def)]);
        }

        // Index fallback
        let indexes = crate::db::package_tree::workspace_indexes(self.db);
        if let Some(defs) = indexes.suffix_index.get(word)
            && let Some(def) = defs.first()
        {
            return Some(vec![ResolvedDefinition::Module(*def)]);
        }

        None
    }

    // util functions for goto_def
    pub fn get_import_at_position(&self, byte_offset: usize) -> Option<&ImportHir<'db>> {
        for import in self.hir.imports(self.db).iter() {
            let span = import.span;
            if byte_offset >= span.start && byte_offset <= span.end {
                return Some(import);
            }
        }
        None
    }

    pub fn resolve_import_pattern(
        &self,
        import: &ImportHir,
    ) -> Option<Vec<ResolvedDefinition<'db>>> {
        let pattern = ImportPattern::new(self.db, import.value.text(self.db).clone());
        let definitions = db::resolve_import(self.db, pattern).as_ref()?;
        Some(
            definitions
                .iter()
                .copied()
                .map(ResolvedDefinition::Module)
                .collect(),
        )
    }

    pub fn find_containing_module(&self, byte_offset: usize) -> Option<&hir::ItemHir<'db>> {
        for item in self.hir.items(self.db).iter() {
            let span = item.span(self.db);
            if byte_offset >= span.start && byte_offset <= span.end {
                return Some(item);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::test_utils::{setup_ned as setup, word_offset};

    // parameter
    #[test]
    fn test_resolve_parameter() {
        let text = r#"
package test.pkg;

simple TestModule {
    parameters:
        int address = 1;
    gates:
        input in;
        output out;
}
"#;
        let (db, ned_file) = setup(text);
        let resolver = ScopeResolver::new(&db, ned_file);
        let ast = db.get_ast_for_file(&ned_file);

        let offset = word_offset(text, "int ", "address");

        let tree = ast.as_ref().expect("Failed to get AST");
        let node = tree
            .root_node()
            .descendant_for_byte_range(offset, offset)
            .expect("node at 'address'");

        let resolved = resolver.resolve(&node, offset, "address");
        assert!(resolved.is_some(), "expected a result for 'address'");
        assert!(
            !resolved.unwrap().is_empty(),
            "'address' should resolve to at least one def"
        );
    }

    // gate

    #[test]
    fn test_resolve_gate_input() {
        let text = r#"
package test.pkg;

simple TestModule {
    parameters:
        int address = 1;
    gates:
        input in;
        output out;
}
"#;
        let (db, ned_file) = setup(text);
        let resolver = ScopeResolver::new(&db, ned_file);
        let ast = db.get_ast_for_file(&ned_file);

        let offset = word_offset(text, "input ", "in");
        let tree = ast.as_ref().expect("Failed to get AST");
        let node = tree
            .root_node()
            .descendant_for_byte_range(offset, offset)
            .expect("node at gate 'in'");

        let resolved = resolver.resolve(&node, offset, "in");
        assert!(resolved.is_some(), "gate 'in' should resolve");
        assert!(
            !resolved.unwrap().is_empty(),
            "gate 'in' should return at least one def"
        );
    }

    #[test]
    fn test_resolve_gate_output() {
        let text = r#"
package test.pkg;

simple TestModule {
    parameters:
        int address = 1;
    gates:
        input in;
        output out;
}
"#;
        let (db, ned_file) = setup(text);
        let resolver = ScopeResolver::new(&db, ned_file);
        let ast = db.get_ast_for_file(&ned_file);

        let offset = word_offset(text, "output ", "out");
        let tree = ast.as_ref().expect("Failed to get AST");
        let node = tree
            .root_node()
            .descendant_for_byte_range(offset, offset)
            .expect("node at gate 'out'");

        let resolved = resolver.resolve(&node, offset, "out");
        assert!(resolved.is_some(), "gate 'out' should resolve");
        assert!(
            !resolved.unwrap().is_empty(),
            "gate 'out' should return at least one def"
        );
    }

    // pattern
    #[test]
    fn test_pattern_resolves_direct_parameter() {
        let text = r#"
package test.pkg;

simple Target {
    parameters:
        int protocol = 2;
}

network TestModule {
    parameters:
        wlan[*].protocol = default("ethernetmac");
    submodules:
        wlan[5]: Target;
}
"#;
        let (db, ned_file) = setup(text);
        let resolver = ScopeResolver::new(&db, ned_file);
        let ast = db.get_ast_for_file(&ned_file);

        let offset = word_offset(text, "wlan[*].", "protocol");
        let tree = ast.as_ref().expect("Failed to get AST");
        let node = tree
            .root_node()
            .descendant_for_byte_range(offset, offset)
            .expect("node at pattern 'protocol'");

        let resolved = resolver.resolve(&node, offset, "protocol");
        assert!(
            resolved.is_some(),
            "pattern 'wlan[*].protocol' should resolve"
        );
        assert!(
            !resolved.unwrap().is_empty(),
            "pattern 'protocol' should return at least one def"
        );
    }

    #[test]
    fn test_pattern_resolves_nested_submodule_parameter() {
        let text = r#"
package test.pkg;

simple Ieee80211Other {
    parameters:
        int registerProtocol = 1;
        int protocol = 2; // should resolve to this param  
}

network TestModule {
    parameters:
        wlan[*].protocol = default("ethernetmac");
        wlan[*].typename = default("Ieee80211Other"); // test for overriding the type
    submodules:
        wlan[5]: <default("Ieee80211")> like IWirelessInterface;
}

module Ieee80211 {
    parameters:
        int protocol; 
    submodules:
        mgmt: Target;
}
"#;
        let (db, ned_file) = setup(text);
        let resolver = ScopeResolver::new(&db, ned_file);
        let ast = db.get_ast_for_file(&ned_file);

        // wlan[*].protocol → should resolve to Ieee80211Other.protocol
        let offset1 = word_offset(text, "wlan[*].", "protocol");
        let tree = ast.as_ref().expect("Failed to get AST");
        let node1 = tree
            .root_node()
            .descendant_for_byte_range(offset1, offset1)
            .expect("node at 'wlan[*].protocol'");

        let resolved1_defs = resolver
            .resolve(&node1, offset1, "protocol")
            .expect("wlan[*].protocol should resolve to something");

        assert!(
            !resolved1_defs.is_empty(),
            "wlan[*].protocol should return at least one def"
        );

        let resolved1_def = resolved1_defs.first().unwrap();
        let span = match resolved1_def {
            ResolvedDefinition::Module(_) => panic!("Expected a member def, got a module"),
            ResolvedDefinition::Member { span, .. } => *span,
        };

        let correct_target_pos = text.find("protocol = 2;").unwrap();
        let wrong_target_pos = text.find("protocol;").unwrap();

        assert_eq!(
            span.start, correct_target_pos,
            "Expected to resolve to the 'protocol' parameter in Ieee80211Other!"
        );

        assert_ne!(
            span.start, wrong_target_pos,
            "Incorrectly resolved to the fallback Ieee80211 module instead of the overridden Ieee80211Other module!"
        );
    }

    // This only works because i am currently treating ** the same as * and the target parameter is
    // directly visible in the first sub context
    #[test]
    fn test_pattern_resolves_wildcard_path_element() {
        let text = r#"
package test.pkg;

simple Target {
    parameters:
        int protocol = 2;
}

network TestModule {
    parameters:
        **.protocol = default(1);
    submodules:
        a: Target;
        b: Target;
}
"#;
        let (db, ned_file) = setup(text);
        let resolver = ScopeResolver::new(&db, ned_file);
        let ast = db.get_ast_for_file(&ned_file);

        let offset = word_offset(text, "**.", "protocol");
        let tree = ast.as_ref().expect("Failed to get AST");
        let node = tree
            .root_node()
            .descendant_for_byte_range(offset, offset)
            .expect("node at '**.protocol'");

        let resolved = resolver.resolve(&node, offset, "protocol");
        assert!(resolved.is_some(), "**.protocol should resolve");
        assert!(
            !resolved.unwrap().is_empty(),
            "**.protocol should return at least one def"
        );
    }

    // Unknown symbol
    #[test]
    fn test_no_result_for_unknown_symbol() {
        let text = r#"
package test.pkg;

simple TestModule {
    parameters:
        int address = 1;
}
"#;
        let (db, ned_file) = setup(text);
        let resolver = ScopeResolver::new(&db, ned_file);
        let ast = db.get_ast_for_file(&ned_file);

        let offset = word_offset(text, "int ", "address");
        let tree = ast.as_ref().expect("Failed to get AST");
        let node = tree
            .root_node()
            .descendant_for_byte_range(offset, offset)
            .expect("node at 'address'");

        let resolved = resolver.resolve(&node, offset, "nonexistent_symbol_xyz");
        assert!(
            resolved.is_none() || resolved.unwrap().is_empty(),
            "unknown symbol should not resolve to anything"
        );
    }
}

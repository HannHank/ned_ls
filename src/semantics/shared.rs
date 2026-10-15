use crate::{
    hir::Diagnostic,
    semantics::{
        ini::NetworkState,
        ned::shared::{ParentScope, match_glob},
    },
};

use std::sync::Arc;

pub struct ValidationCtx<'a, 'db> {
    pub db: &'db dyn crate::db::Db,
    pub diagnostics: &'a mut Vec<Diagnostic>,
    pub version: i32,
}

impl<'a, 'db> ValidationCtx<'a, 'db> {
    pub fn new(
        db: &'db dyn crate::db::Db,
        diagnostics: &'a mut Vec<Diagnostic>,
        version: i32,
    ) -> Self {
        Self {
            db,
            diagnostics,
            version,
        }
    }

    pub fn push(
        &mut self,
        start: usize,
        end: usize,
        message: String,
        severity: crate::hir::Severity,
    ) {
        self.diagnostics.push(Diagnostic::new(
            start,
            end,
            message,
            self.version,
            severity,
            None,
        ));
    }

    pub fn error(&mut self, start: usize, end: usize, message: String) {
        self.push(start, end, message, crate::hir::Severity::Error);
    }

    pub fn warning(&mut self, start: usize, end: usize, message: String) {
        self.push(start, end, message, crate::hir::Severity::Warning);
    }
}

pub struct ContextOverride<'db> {
    pub source_pattern: String,
    pub context: Arc<crate::context::ItemContext<'db>>,
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub struct FlattenedType<'db> {
    pub nodes: Vec<TypeNode<'db>>,
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub struct TypeNode<'db> {
    pub relative_path: String, // tcp, tcp.app
    pub context: std::sync::Arc<crate::context::ItemContext<'db>>,
    pub is_vector: bool, // Needed for the ned files → replaced "resolve_submodules"
    pub vector_size: Option<String>, // same
    pub type_def: Option<crate::db::DefIndex<'db>>,
    pub like_param: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub struct TypeSkeleton<'db> {
    pub nodes: Vec<SkeletonNode<'db>>,
}

#[derive(Clone, Debug, PartialEq, Eq, salsa::SalsaValue)]
pub struct SkeletonNode<'db> {
    pub relative_path: String,
    pub is_vector: bool,
    pub vector_size: Option<String>,
    pub type_def: Option<crate::db::DefIndex<'db>>,
    pub like_param: Option<String>,
}

/// Given a type definition, recursively resolve its structure and return a flattened representation of all submodules and their contexts.
#[salsa::tracked]
pub fn get_type_structure<'db>(
    db: &'db dyn crate::db::Db,
    type_def: crate::db::DefIndex<'db>,
) -> FlattenedType<'db> {
    let mut nodes = Vec::new();

    let resolved_type = crate::context::resolve_item_hir(db, type_def)
        .clone()
        .unwrap();
    let root_ctx =
        crate::context::get_item_context(db, resolved_type.item, resolved_type.definitions.clone());

    // current type itself is always first node
    nodes.push(TypeNode {
        relative_path: String::new(),
        context: root_ctx.clone(),
        is_vector: false,
        vector_size: None,
        type_def: Some(type_def),
        like_param: None,
    });

    let parent_scope = ParentScope {
        params: resolved_type.item.parameters(db),
        context: root_ctx,
    };

    // check direct children
    for (child_name, child_entry) in root_ctx.submodules.iter() {
        let (actual_type_name, _) = crate::semantics::ned::shared::get_submodule_actual_type(
            db,
            &parent_scope,
            child_name,
            &child_entry.item,
        )
        .unwrap_or_default();

        // Detect if this submodules type is driven by a parent parameter
        let child_like_param = match &child_entry.item.definition {
            crate::hir::ned::submodules_hir::SubmoduleDefinition::Parametric {
                like_expr, ..
            } => like_expr.as_ref().and_then(|expr| {
                let (name, is_concrete) =
                    crate::semantics::ned::shared::extract_concrete_type(expr)?;
                if is_concrete && root_ctx.parameters.contains_key(&name) {
                    Some(name)
                } else {
                    None
                }
            }),
            _ => None,
        };

        let is_child_vector = child_entry.item.vector_size_expr.is_some();
        let child_vector_size = child_entry.item.vector_size_expr.clone();

        let child_def_index_opt =
            crate::db::find_def(db, &actual_type_name, &resolved_type.definitions).or_else(|| {
                // If the type is not found in the current context, try to resolve it globally

                // simple name
                if !actual_type_name.contains('.') {
                    let name_word = crate::hir::Word::new(db, actual_type_name.clone());
                    *crate::db::package_tree::resolve_global_type(db, name_word)
                } else {
                    // fqn
                    let pattern = crate::hir::ned::import_hir::ImportPattern::new(
                        db,
                        actual_type_name.clone(),
                    );
                    crate::db::package_tree::resolve_import(db, pattern)
                        .clone()
                        .and_then(|results| results.first().copied())
                }
            });

        // If the child type can be resolved, recursively get its structure and append it to the nodes list
        if let Some(child_def_index) = child_def_index_opt {
            let child_structure = get_type_structure(db, child_def_index);

            for child_node in child_structure.nodes.iter() {
                let (new_path, node_is_vector, node_vector_size, node_like_param) =
                    if child_node.relative_path.is_empty() {
                        (
                            child_name.clone(),
                            is_child_vector,
                            child_vector_size.clone(),
                            child_like_param.clone(),
                        )
                    } else {
                        (
                            format!("{}.{}", child_name, child_node.relative_path),
                            child_node.is_vector,
                            child_node.vector_size.clone(),
                            child_node.like_param.clone(),
                        )
                    };

                nodes.push(TypeNode {
                    relative_path: new_path,
                    context: child_node.context.clone(),
                    is_vector: node_is_vector,
                    vector_size: node_vector_size,
                    type_def: child_node.type_def,
                    like_param: node_like_param,
                });
            }
        } else {
            // push leave node with empty context if type cannot be resolved → this allows the validator to still recognize the path and report missing type errors on it
            let fallback_ctx = crate::context::get_submodule_context_from_type(
                db,
                &actual_type_name,
                resolved_type.definitions.clone(),
            )
            .unwrap_or_else(|| std::sync::Arc::new(crate::context::ItemContext::default()));

            nodes.push(TypeNode {
                relative_path: child_name.clone(),
                context: fallback_ctx,
                is_vector: is_child_vector,
                vector_size: child_vector_size,
                type_def: None,
                like_param: child_like_param.clone(),
            });
        }
    }

    FlattenedType { nodes }
}

/// Given a type definition, return a simplified skeleton representation of its structure, focusing on the relative paths and vector information of submodules.
/// This is useful in order to avoid salsa creating to many dependencies when we only need the
/// structure of the type and not the full context of each submodule.
///
///  1. get_type_structure re-runs → new Arc<ItemContext> values → different result
///  2. get_type_skeleton re-runs → maps to skeleton → same paths, same DefIndexes, same structure → Salsa compares and sees no change
///  3. validate_section → full cache hit, zero work
#[salsa::tracked]
pub fn get_type_skeleton<'db>(
    db: &'db dyn crate::db::Db,
    type_def: crate::db::DefIndex<'db>,
) -> TypeSkeleton<'db> {
    let full = get_type_structure(db, type_def);
    TypeSkeleton {
        nodes: full
            .nodes
            .iter()
            .map(|n| SkeletonNode {
                relative_path: n.relative_path.clone(),
                is_vector: n.is_vector,
                vector_size: n.vector_size.clone(),
                type_def: n.type_def,
                like_param: n.like_param.clone(),
            })
            .collect(),
    }
}

/// Resolve the context for a given SkeletonNode, returning an Arc<ItemContext>.
pub fn resolve_skeleton_context<'db>(
    db: &'db dyn crate::db::Db,
    node: &SkeletonNode<'db>,
) -> Arc<crate::context::ItemContext<'db>> {
    if let Some(def) = node.type_def
        && let Some(resolved) = crate::context::resolve_item_hir(db, def)
    {
        return crate::context::get_item_context(db, resolved.item, resolved.definitions.clone())
            .clone();
    }
    Arc::new(crate::context::ItemContext::default())
}

/// Resolve the vector size for a given size expression, considering both fixed numbers and variable overrides in the network state.
pub fn resolve_vector_size(
    size_expr: &str,
    state: &NetworkState<'_, '_>,
    absolute_node_path: &str,
) -> Option<i32> {
    // fixed number
    if let Ok(val) = size_expr.parse::<i32>() {
        return Some(val);
    }

    // variable defined in ini file
    let parent_path = match absolute_node_path.rfind('.') {
        Some(idx) => &absolute_node_path[..idx],
        None => absolute_node_path,
    };

    // check for overrides in the network state
    if let Some(overrides) = state.param_overrides.get(size_expr) {
        let mut latest_match = None;
        for ov in overrides {
            if match_glob(&ov.clean_pattern, parent_path)
                && let Ok(int_val) = ov.value.trim().parse::<i32>()
            {
                latest_match = Some(int_val);
            }
        }
        if latest_match.is_some() {
            return latest_match;
        }
    }

    None
}

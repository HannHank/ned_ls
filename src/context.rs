// This File groups functions related to building context for name resolution in NED files.
use std::collections::HashMap;
use std::sync::Arc;

use crate::{
    db::{self, DefIndex},
    hir::{self, Word, ned::import_hir::ImportPattern},
};

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct ContextEntry<'db, T> {
    pub item: T,
    pub origin: DefIndex<'db>,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct ItemContext<'db> {
    pub types: HashMap<String, ContextEntry<'db, hir::ItemHir<'db>>>,
    pub parameters: HashMap<String, ContextEntry<'db, hir::ned::ParamBlockItem<'db>>>,
    pub gates: HashMap<String, ContextEntry<'db, hir::ned::GateHir<'db>>>,
    pub submodules: HashMap<String, ContextEntry<'db, hir::ned::submodules_hir::SubmoduleHir<'db>>>,
    pub ancestry: Vec<db::DefIndex<'db>>,
    /// value of each own parameter → the most derived declaration or assignment wins.
    pub param_values: HashMap<String, ContextEntry<'db, hir::ned::ParamBlockItem<'db>>>,
    /// Pattern assignments to parameters of submodules (AutoTNS[*].framePreemption = true;, **.framePreemption = false;)
    /// of the type and its base types
    pub pattern_params: Vec<ContextEntry<'db, hir::ned::ParamBlockItem<'db>>>,
}

impl<'db> ItemContext<'db> {
    pub fn new() -> Self {
        Self {
            parameters: HashMap::new(),
            gates: HashMap::new(),
            submodules: HashMap::new(),
            types: HashMap::new(),
            ancestry: Vec::new(),
            param_values: HashMap::new(),
            pattern_params: Vec::new(),
        }
    }

    /// Checks if a parameter exists in the ancestry chain
    pub fn _is_defined_in_ancestry(&self, db: &'db dyn crate::db::Db, param_name: &str) -> bool {
        for ancestor_def in &self.ancestry {
            if let Some(resolved) = crate::context::resolve_item_hir(db, *ancestor_def)
                && resolved
                    .item
                    .parameters(db)
                    .iter()
                    .any(|p| p.name(db).text(db) == param_name)
            {
                return true;
            };
        }
        false
    }
}

impl Default for ItemContext<'_> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, PartialEq, salsa::SalsaValue)]
pub struct ResolvedItemHir<'db> {
    pub item: hir::ItemHir<'db>,
    pub definitions: Arc<Vec<db::DefIndex<'db>>>,
}

/// Resolves an item to its HIR representation along with the list of definitions that are visible in the file where the item is defined.
#[salsa::tracked]
pub fn resolve_item_hir<'db>(
    db: &'db dyn crate::db::Db,
    def_index: db::DefIndex<'db>,
) -> Option<ResolvedItemHir<'db>> {
    let uri = def_index.uri(db);
    let ned_file = db.get_source_file_input(uri)?;

    let hir_file = hir::ned::lower_to_hir(db, ned_file);
    let target_name = def_index.simple_name(db);
    let defs = db::visible_definitions(db, ned_file);

    for item in hir_file.items(db).iter() {
        if item.name(db) == *target_name {
            return Some(ResolvedItemHir {
                item: *item,
                definitions: defs.clone(),
            });
        }
    }
    None
}

/// Query to compute the full context (Inherited + Local if the item has this info)
#[salsa::tracked]
pub fn get_item_context<'db>(
    db: &'db dyn crate::db::Db,
    item: hir::ItemHir<'db>,
    definitions: Arc<Vec<db::DefIndex<'db>>>,
) -> Arc<ItemContext<'db>> {
    let ancestry = compute_ancestry_chain(db, item, definitions.clone()); // Clone is okay because
    // of salsa ids

    let chain = ancestry.chain(db);

    let mut context = ItemContext::new();

    context.ancestry = chain.clone();

    for ancestor_def in chain {
        if let Some(ancestor_hir) = resolve_item_hir(db, *ancestor_def) {
            merge_into_context(db, &mut context, ancestor_hir.item, *ancestor_def);
        }
    }

    let self_name = item.name(db).text(db);
    let self_def_index_opt = crate::db::find_def(db, self_name, &definitions).or_else(|| {
        let name_word = Word::new(db, self_name.to_string());
        *crate::db::package_tree::resolve_global_type(db, name_word)
    });
    if let Some(self_def_index) = self_def_index_opt {
        merge_into_context(db, &mut context, item, self_def_index);
    } else {
        let name_word = Word::new(db, self_name.to_string());
        if let Some(first_def) = definitions.first() {
            let local_uri = first_def.uri(db).clone();
            let local_def =
                crate::db::DefIndex::new(db, name_word, name_word, local_uri, item.item_to_def());
            merge_into_context(db, &mut context, item, local_def);
        }
    }
    Arc::new(context)
}

// Maybe I can implement some traid t for merging contexts?
/// Merges the contents of an ItemHir into an existing ItemContext
pub fn merge_into_context<'db>(
    db: &'db dyn crate::db::Db,
    ctx: &mut ItemContext<'db>,
    item: hir::ItemHir<'db>,
    origin: db::DefIndex<'db>,
) {
    // TODO: I think it would be also cool to check if something gets overwriten and throw a
    // warning
    for param in item.parameters(db) {
        let name = match param {
            hir::ned::ParamBlockItem::Param(p) => {
                // Assignments are needed later to tell whether INI can still override a value
                if *p.pattern(db) {
                    ctx.pattern_params.push(ContextEntry {
                        item: param.clone(),
                        origin,
                    });
                    continue;
                }
                if p.value(db).is_some() {
                    ctx.param_values.insert(
                        p.name(db).text(db).to_string(),
                        ContextEntry {
                            item: param.clone(),
                            origin,
                        },
                    );
                }

                // Avoid overwriting definitions
                if p.ned_type(db).is_none() {
                    continue;
                }
                p.name(db).text(db)
            }

            hir::ned::ParamBlockItem::Property(p) => p.name(db).text(db),
        };
        let entry = ContextEntry {
            item: param.clone(),
            origin,
        };
        ctx.parameters.insert(name.to_string(), entry);
    }

    for local_type in item.types(db) {
        let name = local_type.name(db).text(db);
        let entry = ContextEntry {
            item: *local_type,
            origin,
        };
        ctx.types.insert(name.to_string(), entry);
    }

    for gate in item.gates(db) {
        let name = gate.name.text(db);
        let entry = ContextEntry {
            item: *gate,
            origin,
        };
        ctx.gates.insert(name.to_string(), entry);
    }

    for sub in item.submodules(db) {
        let name = sub.name.clone();
        let entry = ContextEntry {
            item: sub.clone(),
            origin,
        };
        ctx.submodules.insert(name.to_string(), entry);
    }
}

#[salsa::tracked]
pub struct AncestryChain<'db> {
    pub chain: Vec<db::DefIndex<'db>>,
}

/// Computes the full ancestry chain of an item by traversing its extends references recursively.
#[salsa::tracked]
pub fn compute_ancestry_chain<'db>(
    db: &'db dyn crate::db::Db,
    start_module: hir::ItemHir<'db>,
    definitions: Arc<Vec<db::DefIndex<'db>>>,
) -> AncestryChain<'db> {
    let start_node = start_module;

    // To get the full ancestry chain, I tried implementing DFS with cycle detection
    let mut chain = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut on_stack = std::collections::HashSet::new();

    fn visit<'db>(
        db: &'db dyn crate::db::Db,
        current: hir::ItemHir<'db>,
        definitions: &[db::DefIndex<'db>],
        visited: &mut std::collections::HashSet<db::DefIndex<'db>>,
        on_stack: &mut std::collections::HashSet<db::DefIndex<'db>>,
        chain: &mut Vec<db::DefIndex<'db>>, // TODO: Create visit result type
    ) {
        let extends_list = current.extends(db);

        for extends_ref in extends_list {
            let name = extends_ref.name.text(db);

            // Resolve Name → DefIndex
            if let Some(def_index) = db::find_def(db, name, definitions) {
                if on_stack.contains(&def_index) {
                    return;
                }

                if visited.contains(&def_index) {
                    continue;
                }

                // I saw this pattern somewhere else to manage the recursion stack and it seems to work
                // well
                on_stack.insert(def_index);
                if let Some(resolved_parent) = resolve_item_hir(db, def_index) {
                    visit(
                        db,
                        resolved_parent.item,
                        &resolved_parent.definitions,
                        visited,
                        on_stack,
                        chain,
                    );
                }
                on_stack.remove(&def_index);

                visited.insert(def_index);
                chain.push(def_index);
            }
        }
    }

    // Start traversal from the start node
    visit(
        db,
        start_node,
        &definitions,
        &mut visited,
        &mut on_stack,
        &mut chain,
    );

    AncestryChain::new(db, chain)
}

pub fn combine_contexts<'db>(from: &ItemContext<'db>, into: &mut ItemContext<'db>) {
    // avoids new memory allocations by reusing the existing hashmaps in the target context
    into.types
        .extend(from.types.iter().map(|(k, v)| (k.clone(), v.clone())));
    into.parameters
        .extend(from.parameters.iter().map(|(k, v)| (k.clone(), v.clone())));
    into.gates
        .extend(from.gates.iter().map(|(k, v)| (k.clone(), v.clone())));
    into.submodules
        .extend(from.submodules.iter().map(|(k, v)| (k.clone(), v.clone())));
    into.param_values.extend(
        from.param_values
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    into.pattern_params
        .extend(from.pattern_params.iter().cloned());
}

// TODO: the context of a submodule is not the outer context but the context of the type it refers
// to which could be either a local type or an imported one
// Are local types able to extend other types?
pub fn get_submodule_context_from_type<'db>(
    db: &'db dyn crate::db::Db,
    sub_type_name: &str,
    current_file_definitions: Arc<Vec<db::DefIndex<'db>>>,
) -> Option<Arc<ItemContext<'db>>> {
    // INFO: opp allows to use simple names inside the typename parameter even if they are not
    // imported → forcing an fqn seems like a better design decision since it is more consistent with the rest of the language and allows to easily distinguish between local and global types. For now, I will implement a fallback that tries to resolve simple names globally if the direct lookup fails.
    let type_def_index = crate::db::find_def(db, sub_type_name, &current_file_definitions)
        .or_else(|| {
            if !sub_type_name.contains('.') {
                let name_word = crate::hir::Word::new(db, sub_type_name.to_string());
                *crate::db::package_tree::resolve_global_type(db, name_word)
            } else {
                let pattern = ImportPattern::new(db, sub_type_name.to_string());
                crate::db::package_tree::resolve_import(db, pattern)
                    .as_ref()
                    .and_then(|results| results.first().copied())
            }
        })?;

    let resolved_type = resolve_item_hir(db, type_def_index).as_ref()?;

    Some(get_item_context(db, resolved_type.item, resolved_type.definitions.clone()).clone())
}

use std::sync::Arc;

// Small wrapper to retrieve the interfaces of an Ned item
use crate::{
    context::{ItemContext, combine_contexts, get_item_context, resolve_item_hir},
    db,
};

#[allow(dead_code)]
pub fn resolve_interfaces<'db>(
    db: &'db dyn db::Db,
    items: &Vec<db::DefIndex<'db>>, // Interfaces
) -> Arc<ItemContext<'db>> {
    let mut module_interface_context = ItemContext::new();
    // Fetch context for each item
    for interface_def in items {
        let interface_hir_opt = resolve_item_hir(db, *interface_def);
        if let Some(interface_hir) = interface_hir_opt {
            let interface_context =
                get_item_context(db, interface_hir.item, interface_hir.definitions.clone());
            combine_contexts(interface_context, &mut module_interface_context);
        }
    }
    Arc::new(module_interface_context)
}

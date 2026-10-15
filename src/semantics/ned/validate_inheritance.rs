use crate::hir::{self, Diagnostic, ItemHir};

pub fn validate_inheritance<'db>(
    db: &'db dyn crate::db::Db,
    item: &'db ItemHir<'db>,
    definitions: &[crate::db::DefIndex<'db>],
    ned_file: hir::SourceFile,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    let extends_list = item.extends(db);

    // Multiple Inheritance
    if extends_list.len() > 1 {
        match item {
            ItemHir::ModuleInterface(_) /* | ItemHir::ChannelInterface(_)*/ => {
                // Allowed for Interfaces
            }
            _ => {
                // Illegal for Modules
                for extra_extend in &extends_list[1..] {
                    diagnostics.push(Diagnostic::new(
                        extra_extend.span.start,
                        extra_extend.span.end,
                        format!("Multiple inheritance is not allowed for {}. Only Interfaces and Channels support it.", item.item_to_def()),
                        *ned_file.version(db),
                        crate::hir::Severity::Error,
None,
                    ));
                }
            }
        }
    }

    // Type Compatibility
    for extends_ref in extends_list {
        let name = extends_ref.name.text(db);

        // Resolve parent
        if let Some(parent_def) = crate::db::find_def(db, name, definitions)
            && let Some(resolved_parent) = crate::context::resolve_item_hir(db, parent_def)
        {
            let parent_hir = resolved_parent.item;
            let is_valid = match (&item, &parent_hir) {
                // Valid Pairs
                (ItemHir::SimpleModule(_), ItemHir::SimpleModule(_)) => true,
                (ItemHir::CompoundModule(_), ItemHir::CompoundModule(_)) => true,
                (ItemHir::CompoundModule(_), ItemHir::SimpleModule(_)) => true,
                (ItemHir::ModuleInterface(_), ItemHir::ModuleInterface(_)) => true,
                (ItemHir::Network(_), ItemHir::Network(_)) => true,
                (ItemHir::Channel(_), ItemHir::Channel(_)) => true,
                _ => false,
            };

            if !is_valid {
                let parent_kind = match parent_hir {
                    ItemHir::SimpleModule(_) => "Simple Module",
                    ItemHir::CompoundModule(_) => "Compound Module",
                    ItemHir::ModuleInterface(_) => "Module Interface",
                    ItemHir::Network(_) => "Network",
                    ItemHir::Channel(_) => "Channel",
                    // _ => "Unknown",
                };

                diagnostics.push(Diagnostic::new(
                    extends_ref.span.start,
                    extends_ref.span.end,
                    format!(
                        "Inheritance mismatch: {} cannot extend {}.",
                        item.item_to_def(),
                        parent_kind
                    ),
                    *ned_file.version(db),
                    crate::hir::Severity::Error,
                    None,
                ));
            }
        }
    }

    diagnostics
}

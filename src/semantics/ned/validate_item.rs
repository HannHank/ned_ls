use std::sync::Arc;

use crate::{
    context::get_item_context,
    db::DefIndex,
    hir::{self, ItemHir, ned::ParamBlockItem},
    semantics::shared::ValidationCtx,
};

use super::resolve_interfaces::resolve_interfaces;
use super::shared::*;

// validation steps for an item:
use super::validate_connections::validate_connections;
use super::validate_extends::validate_extends;
use super::validate_implements::validate_implements;
use super::validate_inheritance::validate_inheritance;
use super::validate_interfaces::validate_interfaces;
use super::validate_parameters::validate_parameters;
use super::validate_submodules::validate_submodules;

#[salsa::tracked]
pub fn validate_item<'db>(
    db: &'db dyn crate::db::Db,
    ned_file: hir::SourceFile,
    definitions: Arc<Vec<DefIndex<'db>>>,
    item: ItemHir<'db>,
) -> Vec<hir::Diagnostic> {
    let mut diagnostics = Vec::new();

    // eprintln!("Local submodules: {:#?}", item.submodules(db).iter().map(|s| s.name.clone()).collect::<Vec<_>>());
    // Check Inheritance
    let inheritance_errors = validate_inheritance(db, &item, &definitions, ned_file);
    diagnostics.extend(inheritance_errors);

    let context = get_item_context(db, item, definitions.clone());

    let version = ned_file.version(db);

    let mut ctx = ValidationCtx::new(db, &mut diagnostics, *version);

    let item_owned = item;
    let scope = ValidationScope::new(&item_owned, context.clone(), definitions.clone());

    // Validate Extends and Implements
    validate_extends(&mut ctx, &scope);
    let likes_def = validate_implements(&mut ctx, &scope);

    // Resolve Interface Context
    let interface_context = resolve_interfaces(db, &likes_def);

    // Extract local definitions (Inner types)
    let mut local_definitions = Vec::new();
    let local_types = context.types.clone();
    for (name, local_type) in local_types.iter() {
        let name_word = hir::Word::new(db, name.clone());
        local_definitions.push(DefIndex::new(
            db,
            name_word,
            name_word,
            ned_file.uri(db).clone(),
            local_type.item.item_to_def(),
        ));
    }

    let item_name = item.name(db).text(db);
    let current_def_index = definitions
        .iter()
        .find(|d| d.simple_name(db).text(db) == item_name)
        .copied()
        .expect("Current item must exist in the file's definitions");

    // General Validations
    validate_parameters(&mut ctx, &scope, current_def_index);
    validate_interfaces(&mut ctx, &scope, &interface_context);

    // Specific Validations based on type
    match item {
        ItemHir::CompoundModule(_) | ItemHir::Network(_) => {
            validate_submodules(&mut ctx, &scope, &local_definitions, current_def_index);
            validate_connections(&mut ctx, &scope);
        }
        ItemHir::ModuleInterface(_) => {
            // ModuleInterface specific validations
            for param in item.parameters(db) {
                if let ParamBlockItem::Param(_p) = param {
                    // if _p.value(db).is_some() {
                    //     diagnostics.push(hir::Diagnostic::new(
                    //         param.span(db).start,
                    //         param.span(db).end,
                    //         "Parameters in moduleinterface are not allowed to have default values".to_string(),
                    //         version,
                    //         None,
                    //     ));
                    // }
                }
            }
        }
        ItemHir::SimpleModule(_) => {
            // Specific validation
            validate_connections(&mut ctx, &scope);
        }
        ItemHir::Channel(_) => {} // TODO:
    }

    diagnostics
}

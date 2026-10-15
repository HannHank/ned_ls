use crate::semantics::ned::prelude::*;

pub fn find_interface_span<'db>(
    db: &'db dyn crate::db::Db,
    item: &ItemHir<'db>,
    target_name: &str,
) -> crate::hir::Span {
    let likes_list = item.like(db);
    let interface_like = likes_list
        .iter()
        .find(|like| like.name.text(db) == target_name);
    if let Some(like) = interface_like {
        like.span
    } else {
        item.span(db)
    }
}

pub fn validate_interfaces<'a, 'db>(
    ctx: &mut ValidationCtx<'_, 'db>,
    scope: &ValidationScope<'a, 'db>,
    interface_context: &ItemContext<'db>,
) {
    // Check parameters
    for (param_name, interface_entry) in interface_context.parameters.iter() {
        let interface_p = match &interface_entry.item {
            ParamBlockItem::Param(p) => p,
            _ => continue,
        };

        let context_entry_opt = scope.context.parameters.get(param_name);

        match context_entry_opt {
            Some(context_entry) => {
                if let ParamBlockItem::Param(context_p) = &context_entry.item {
                    let iface_type_opt = interface_p.ned_type(ctx.db);
                    let impl_type_opt = context_p.ned_type(ctx.db);

                    if let (Some(iface_type), Some(impl_type)) = (iface_type_opt, impl_type_opt)
                        && !iface_type.accepts(impl_type)
                    {
                        let message = format!(
                            "Parameter '{}' type mismatch. Interface '{}' expects '{}', but found '{}'.",
                            param_name,
                            interface_entry.origin.simple_name(ctx.db).text(ctx.db),
                            iface_type.as_str(),
                            impl_type.as_str()
                        );

                        ctx.error(
                            context_p.span(ctx.db).start,
                            context_p.span(ctx.db).end,
                            message,
                        );
                    }
                }
            }
            None => {
                let message = format!(
                    "Parameter '{}' required by interface '{}' is missing from {}",
                    param_name,
                    interface_entry.origin.simple_name(ctx.db).text(ctx.db),
                    scope.item.kind()
                );

                let target_name = interface_entry.origin.simple_name(ctx.db);
                let span = find_interface_span(ctx.db, scope.item, target_name.text(ctx.db));
                ctx.error(span.start, span.end, message);
            }
        }
    }

    // Check gates
    for (gate_name, interface_entry) in interface_context.gates.iter() {
        let interface_gate = &interface_entry.item;
        let context_entry_opt = scope.context.gates.get(gate_name);

        match context_entry_opt {
            Some(context_entry) => {
                let context_gate = &context_entry.item;

                if interface_gate.direction != context_gate.direction {
                    let message = format!(
                        "Gate '{}' direction mismatch. Interface '{}' expects '{:?}', but found '{:?}'.",
                        gate_name,
                        interface_entry.origin.simple_name(ctx.db).text(ctx.db),
                        interface_gate.direction,
                        context_gate.direction
                    );

                    ctx.error(context_gate.span.start, context_gate.span.end, message);
                }

                if interface_gate.is_vector != context_gate.is_vector {
                    let expected_str = if interface_gate.is_vector {
                        "vector []"
                    } else {
                        "scalar"
                    };
                    let found_str = if context_gate.is_vector {
                        "vector []"
                    } else {
                        "scalar"
                    };

                    let message = format!(
                        "Gate '{}' vector mismatch. Interface '{}' expects {}, but found {}.",
                        gate_name,
                        interface_entry.origin.simple_name(ctx.db).text(ctx.db),
                        expected_str,
                        found_str
                    );

                    ctx.error(
                        scope.item.span(ctx.db).start,
                        scope.item.span(ctx.db).end,
                        message,
                    );
                }
            }
            None => {
                let message = format!(
                    "Gate '{}' required by interface '{}' is missing from {}.",
                    gate_name,
                    interface_entry.origin.simple_name(ctx.db).text(ctx.db),
                    scope.item.kind()
                );
                let target_name = interface_entry.origin.simple_name(ctx.db);
                let span = find_interface_span(ctx.db, scope.item, target_name.text(ctx.db));

                ctx.error(span.start, span.end, message);
            }
        }
    }
}

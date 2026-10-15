use crate::{
    hir::ned::{
        GateDirection,
        connections_hir::{ArrowKind, ChannelSpec, ConnectionEndpoint},
        submodules_hir::SubmoduleDefinition,
    },
    semantics::ned::prelude::*,
};

/// This function validates the connections of a module or network by checking if the referenced gates exist and if their directions are compatible with the connection arrows. It also validates any channel specifications associated with the connections.
pub fn validate_connections<'a, 'db>(
    ctx: &mut ValidationCtx<'a, 'db>,
    scope: &ValidationScope<'a, 'db>,
) {
    let connections = match scope.item {
        ItemHir::CompoundModule(m) => m.connections(ctx.db),
        ItemHir::Network(n) => n.connections(ctx.db),
        _ => return,
    };

    for conn in connections {
        validate_endpoint(ctx, scope, &conn.src, true, &conn.arrow);
        validate_endpoint(ctx, scope, &conn.dest, false, &conn.arrow);

        if let Some(channel) = &conn.channel {
            validate_channel(ctx, scope, channel);
        }
    }
}

/// This function validates a connection endpoint by checking if the referenced gate exists and if its direction is compatible with the connection arrow.
fn validate_endpoint<'a, 'db>(
    ctx: &mut ValidationCtx<'a, 'db>,
    scope: &ValidationScope<'a, 'db>,
    endpoint: &ConnectionEndpoint<'db>,
    is_src: bool,
    arrow: &ArrowKind,
) {
    let mut target_context = None;
    let mut is_submodule = false;

    // First check if the module is available
    if let Some(module_word) = &endpoint.module_name {
        let module_name = module_word.text(ctx.db);
        is_submodule = true;
        if let Some(sub_entry) = scope.context.submodules.get(module_name) {
            // If it is a like entry, we just need to care about the interface
            match sub_entry.item.definition {
                SubmoduleDefinition::Parametric {
                    ref interface_name, ..
                } => {
                    // get the interface and check if the gate exists there
                    if let Some(resolved_ctx) = crate::context::get_submodule_context_from_type(
                        ctx.db,
                        interface_name,
                        scope.arc_defs.clone(),
                    ) {
                        target_context = Some(resolved_ctx);
                    }
                }
                _ => {
                    let submodule = &sub_entry.item;

                    let parent = ParentScope {
                        params: scope.item.parameters(ctx.db),
                        context: &scope.context,
                    };
                    // Modules can be further concretized by a "typename" parameter assignment
                    let actual_type =
                        get_submodule_actual_type(ctx.db, &parent, module_name, submodule)
                            .map(|(t, _)| t)
                            .unwrap_or_default();

                    // Check if actual_type is a local inner type first
                    if let Some(local_type) = scope.context.types.get(&actual_type) {
                        target_context = Some(
                            crate::context::get_item_context(
                                ctx.db,
                                local_type.item,
                                scope.arc_defs.clone(),
                            )
                            .clone(),
                        );
                    } else if let Some(resolved_ctx) =
                        crate::context::get_submodule_context_from_type(
                            ctx.db,
                            &actual_type,
                            scope.arc_defs.clone(),
                        )
                    {
                        target_context = Some(resolved_ctx);
                    }
                }
            }
        } else {
            // We cannot find the Submodule in the current context
            ctx.error(
                endpoint.span.start,
                endpoint.span.end,
                format!("Submodule '{}' not found.", module_name),
            );
            return;
        }
    } else {
        target_context = Some(scope.context.clone());
    }

    //
    //
    let gate_name = endpoint.gate_name.text(ctx.db);
    let mut found_direction = None;
    let mut found_is_vector = false;

    if let Some(tc) = target_context {
        let base_gate_name = gate_name.replace("$i", "").replace("$o", "");

        if let Some(gate_entry) = tc.gates.get(&base_gate_name) {
            found_direction = Some(gate_entry.item.direction);
            found_is_vector = gate_entry.item.is_vector;
        } else {
            let msg = if is_submodule {
                // eprintln!("Submodule: {:?}", tc);
                format!(
                    "Gate '{}' not defined for submodule '{}'.",
                    gate_name,
                    endpoint.module_name.as_ref().unwrap().text(ctx.db)
                )
            } else {
                format!("Gate '{}' not found.", gate_name)
            };
            ctx.error(endpoint.span.start, endpoint.span.end, msg);
            return;
        }
    }

    // I find it nice to have a small error message for the user if they use an index or ++ on a non-vector gate or if they forget to use it on a vector gate
    if found_is_vector && !endpoint.is_plusplus && !endpoint.has_index {
        ctx.error(
            endpoint.span.start,
            endpoint.span.end,
            format!(
                "Gate '{}' is a vector gate, which requires an index '[]' or '++'.",
                gate_name
            ),
        );
    } else if !found_is_vector && (endpoint.is_plusplus || endpoint.has_index) {
        ctx.error(
            endpoint.span.start,
            endpoint.span.end,
            format!("Gate '{}' is not a vector gate.", gate_name),
        );
    }

    if let Some(dir) = found_direction {
        // The rules for valid connections are as follows:
        let is_valid = match (is_src, is_submodule, arrow, &dir) {
            (_, _, ArrowKind::Bidirectional, GateDirection::Inout) => true,
            (_, _, ArrowKind::Bidirectional, _) => false,

            (true, true, ArrowKind::Right, GateDirection::Output | GateDirection::Inout) => true,
            (true, false, ArrowKind::Right, GateDirection::Input | GateDirection::Inout) => true,

            (false, true, ArrowKind::Right, GateDirection::Input | GateDirection::Inout) => true,
            (false, false, ArrowKind::Right, GateDirection::Output | GateDirection::Inout) => true,

            (true, true, ArrowKind::Left, GateDirection::Input | GateDirection::Inout) => true,
            (true, false, ArrowKind::Left, GateDirection::Output | GateDirection::Inout) => true,

            (false, true, ArrowKind::Left, GateDirection::Output | GateDirection::Inout) => true,
            (false, false, ArrowKind::Left, GateDirection::Input | GateDirection::Inout) => true,

            _ => false,
        };

        if !is_valid {
            let expected = match (is_src, is_submodule, arrow) {
                (_, _, ArrowKind::Bidirectional) => "inout",
                (true, true, ArrowKind::Right) | (false, false, ArrowKind::Right) => "output",
                (false, true, ArrowKind::Right) | (true, false, ArrowKind::Right) => "input",
                (true, true, ArrowKind::Left) | (false, false, ArrowKind::Left) => "input",
                (false, true, ArrowKind::Left) | (true, false, ArrowKind::Left) => "output",
            };

            ctx.error(
                endpoint.span.start,
                endpoint.span.end,
                format!(
                    "Direction mismatch: Expected {} gate, but found {:?} gate.",
                    expected, dir
                ),
            );
        }
    }
}

/// This function validates a channel specification by checking if the referenced channel type exists in the local types or any imported packages.
fn validate_channel<'a, 'db>(
    ctx: &mut ValidationCtx<'a, 'db>,
    scope: &ValidationScope<'a, 'db>,
    channel: &ChannelSpec<'db>,
) {
    if let Some(channel_type_word) = &channel.channel_type {
        let raw_type = channel_type_word.text(ctx.db);

        // Extract the actual concrete type, stripping `default("...")` if present
        if let Some((name, is_concrete)) = extract_concrete_type(raw_type) {
            // If the expression was fully dynamic, we safely skip strict validation
            if !is_concrete {
                return;
            }

            use crate::db::package_tree::DefKind;
            let mut is_locally_defined = false;

            // Check local types first
            if let Some(local_type) = scope.context.types.get(&name) {
                let kind = local_type.item.item_to_def();
                if kind != DefKind::Channel && kind != DefKind::ChannelInterface {
                    ctx.error(
                        channel.span.start,
                        channel.span.end,
                        format!("'{}' is a {}, not a channel.", name, kind),
                    );
                }
                is_locally_defined = true;
            }

            if !is_locally_defined {
                if let Some(def_index) = crate::db::find_def(ctx.db, &name, &scope.arc_defs) {
                    let kind = def_index.kind(ctx.db);
                    if *kind != DefKind::Channel && *kind != DefKind::ChannelInterface {
                        ctx.error(
                            channel.span.start,
                            channel.span.end,
                            format!("'{}' is a {}, not a channel.", name, kind),
                        );
                    }
                } else {
                    ctx.error(
                        channel.span.start,
                        channel.span.end,
                        format!(
                            "Channel '{}' not found in local types or any imported packages.",
                            name
                        ),
                    );
                }
            }
        }
    }
}

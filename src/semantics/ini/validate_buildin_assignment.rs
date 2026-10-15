use super::prelude::*;
use crate::semantics::ini::builtin_config;

/// Validates a built-in assignment, checking if the target parameter name is known and reporting an error if it is not.
pub fn validate_builtin_assignment<'db>(
    ctx: &mut ValidationCtx<'_, 'db>,
    section_chain: &[crate::hir::ini::section_hir::IniSectionHir<'db>],
    assignment: &crate::hir::ini::assignment_hir::IniAssignmentHir<'_>,
    segments: &[super::ParsedIniSegment],
    target_param_name: &str,
) {
    let is_global = segments.is_empty();

    // some keywords are only valid in the global section
    let opt = if is_global {
        builtin_config::lookup_global(target_param_name)
    } else {
        builtin_config::lookup_per_object(target_param_name)
            .or_else(|| builtin_config::lookup_global(target_param_name))
    };

    // check if we know the key
    let Some(opt) = opt else {
        let scope = if is_global { "global " } else { "" };
        ctx.error(
            assignment.key_span.start,
            assignment.key_span.end,
            format!(
                "Unknown {}configuration option '{}'.",
                scope, target_param_name
            ),
        );
        return;
    };

    // check if the index of the seed is correct
    check_seed_rng_index(ctx, section_chain, target_param_name, &assignment.key_span);
    let value = assignment.value.text(ctx.db).clone();
    check_builtin_value_type(ctx, opt, &value, &assignment.value_span);
}

/// Checks value of build-in types. Very simple and not strict.
pub fn check_builtin_value_type(
    ctx: &mut ValidationCtx<'_, '_>,
    opt: &builtin_config::BuiltinConfigOption,
    raw_value: &str,
    value_span: &crate::hir::Span,
) {
    use builtin_config::ConfigValueType;

    let value = raw_value.trim();
    if value.is_empty() || value.contains('$') || value.contains('(') {
        return;
    }

    // check if the value matches the expected type
    let ok = match opt.value_type {
        ConfigValueType::Bool => value == "true" || value == "false",
        ConfigValueType::Int => value.parse::<i64>().is_ok(),
        ConfigValueType::Double => value.parse::<f64>().is_ok(),
        ConfigValueType::Quantity(_) => is_quantity_literal(value),
        ConfigValueType::String => true,
    };
    // all good → no warning
    if ok {
        return;
    }

    // report a warning if the value does not match the expected type
    let expected = match opt.value_type {
        ConfigValueType::Bool => "true or false".to_string(),
        ConfigValueType::Int => "an integer".to_string(),
        ConfigValueType::Double => "a number".to_string(),
        ConfigValueType::Quantity(unit) => format!("a quantity such as 100{}", unit),
        ConfigValueType::String => unreachable!(),
    };
    ctx.warning(
        value_span.start,
        value_span.end,
        format!("Option '{}' expects {}.", opt.name, expected),
    );
}

/// Bad check for quantity literals. TODO:
fn is_quantity_literal(value: &str) -> bool {
    let Some(first) = value.chars().next() else {
        return false;
    };
    (first.is_ascii_digit() || matches!(first, '.' | '+' | '-'))
        && value.chars().any(|c| c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | ' '))
}

/// Warns when a seed options rng index is >= num-rngs INFO: opp ignores this
pub fn check_seed_rng_index<'db>(
    ctx: &mut ValidationCtx<'_, 'db>,
    section_chain: &[crate::hir::ini::section_hir::IniSectionHir<'db>],
    key_name: &str,
    key_span: &crate::hir::Span,
) {
    let Some(rng_index) = builtin_config::seed_rng_index(key_name) else {
        return;
    };
    if let Some(num_rngs) = effective_num_rngs(ctx.db, section_chain)
        && rng_index >= num_rngs
    {
        ctx.warning(
            key_span.start,
            key_span.end,
            format!(
                "RNG index {} is out of range (num-rngs is {}), so this seed is ignored.",
                rng_index, num_rngs
            ),
        );
    }
}

/// Resolve num-rngs from the section chain.
pub fn effective_num_rngs<'db>(
    db: &'db dyn crate::db::Db,
    section_chain: &[crate::hir::ini::section_hir::IniSectionHir<'db>],
) -> Option<u32> {
    let mut effective: Option<String> = None;
    for section in section_chain {
        for config in section.configs(db) {
            if config.key.text(db) == "num-rngs" {
                // store the result
                effective = Some(config.value.text(db).clone());
            }
        }
    }
    match effective {
        None => Some(1), // default value if nothing is specified
        Some(value) => value.trim().parse().ok(),
    }
}

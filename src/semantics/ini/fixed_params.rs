// Try to detect INI assignments wihtout effects → NED assigns without a default() are
// fixed values and cannot be overridden by INI.
//
// INFO: three different cases I can think of:
// 1. simple int test =1;
// 2. host: Host { test = 2; }
// 3. pattern **.test = 3;

use std::sync::Arc;

use super::prelude::*;
use crate::context::ItemContext;
use crate::hir::ned::{ParamBlockItem, ParamHir, ValueKind};

fn fixed_location(
    db: &dyn crate::db::Db,
    item: &ParamBlockItem,
    location: impl FnOnce() -> String,
) -> Option<String> {
    let ParamBlockItem::Param(p) = item else {
        return None;
    };
    (*p.value_kind(db) == Some(ValueKind::Fixed)).then(location)
}

fn type_name<'db>(db: &'db dyn crate::db::Db, def: crate::db::DefIndex<'db>) -> &'db str {
    def.simple_name(db).text(db)
}

/// Check if a ned pattern assignment applies to a parameter of a submodule, and if it does, return the location of that assignment.
fn pattern_covers(db: &dyn crate::db::Db, p: &ParamHir, rel_path: &str, param: &str) -> bool {
    // the pattern must match the parameter name
    let Some(target) = p.target_param(db) else {
        return false;
    };
    let target = target.text(db);
    let target_matches = if target.contains('*') || target.contains('?') {
        match_glob(target, param)
    } else {
        target == param
    };
    if !target_matches {
        return false;
    }

    let mut names = Vec::new();
    // the pattern must match the path down to the submodule, and must not contain any index →
    // with an index, the assignment would only apply to a specific submodule instance, and not to all of them.
    for segment in p.segments(db) {
        // println!("segment: {} (index={:?})", segment.name(db).text(db), segment.index(db));
        if let Some(index) = segment.index(db)
            && index.text(db).trim() != "*"
        {
            return false;
        }
        names.push(segment.name(db).text(db).as_str());
    }

    !names.is_empty() && match_glob(&names.join("."), rel_path)
}

/// Fixed assignment of an enclosing module context for the module at below → the path from the
/// enclosing module down to it (starting with the direct submodule).
fn fixed_by_enclosing(
    db: &dyn crate::db::Db,
    ctx: &ItemContext,
    below: &[&str],
    param: &str,
) -> Option<String> {
    // pattern assignments in the parameters of the enclosing module
    let rel_path = below.join(".");
    // the first pattern that matches the path down to the submodule and the parameter name wins →
    // one match is enough
    let first_pattern = ctx.pattern_params.iter().find(|entry| {
        matches!(&entry.item, ParamBlockItem::Param(p) if pattern_covers(db, p, &rel_path, param))
    });

    // eprintln!("fixed_by_enclosing: ctx={} below={:?} param={} first_pattern={:?}", type_name(db, ctx.origin), below, param, first_pattern.map(|e| e.item.text(db)));
    if let Some(entry) = first_pattern
        && let ParamBlockItem::Param(p) = &entry.item
        && let Some(location) = fixed_location(db, &entry.item, || {
            format!(
                "by '{}' in {}",
                p.name(db).text(db),
                type_name(db, entry.origin)
            )
        })
    {
        return Some(location);
    }

    // assignments in the body of the submodule declaration on the path
    let child = ctx.submodules.get(below[0])?;
    let rest = below[1..].join(".");
    let first_in_body = child.item.parameters.iter().find(|item| match item {
        // a pattern assignment in the submodule body only applies to the parameter if it matches the rest of the path down to the submodule
        ParamBlockItem::Param(p) if *p.pattern(db) => {
            !rest.is_empty() && pattern_covers(db, p, &rest, param)
        }
        ParamBlockItem::Param(p) => rest.is_empty() && p.name(db).text(db) == param,
        ParamBlockItem::Property(_) => false,
    })?;
    fixed_location(db, first_in_body, || {
        format!(
            "in the body of submodule '{}' in {}",
            below[0],
            type_name(db, child.origin)
        )
    })
}

/// Where NED assigns param of the module at absolute_path without default(), or None if an
/// INI value can still take effect.
/// module_ctx is the active context of that module
pub fn ned_fixed_assignment<'db>(
    db: &'db dyn crate::db::Db,
    network_state: &super::NetworkState<'_, 'db>,
    absolute_path: &str,
    module_ctx: &ItemContext<'db>,
    param: &str,
) -> Option<String> {
    // own type → the most derived value wins
    if let Some(entry) = module_ctx.param_values.get(param)
        && let Some(location) = fixed_location(db, &entry.item, || {
            format!("in {}", type_name(db, entry.origin))
        })
    {
        return Some(location);
    }

    // every enclosing module, from the network down to the parent
    let elems: Vec<&str> = absolute_path.split('.').collect();
    for depth in 0..elems.len().saturating_sub(1) {
        let enclosing_path = elems[..=depth].join(".");

        // TODO: For now I want to skip modules that are replaced by the INI file → for now I just
        // skip any module that has a dynamic context
        if network_state.dynamic_contexts.contains_key(&enclosing_path) {
            return None;
        }

        let Some(node) =
            super::validate_assignment::find_node_for_path(network_state, &enclosing_path)
        else {
            continue;
        };
        let ctx: Arc<ItemContext<'db>> = network_state.get_cached_context(db, node);
        if let Some(location) = fixed_by_enclosing(db, &ctx, &elems[depth + 1..], param) {
            return Some(location);
        }
    }

    None
}

// TODO: replace generated tests later
#[cfg(test)]
mod tests {
    use crate::semantics::ini::semantic_analysis;
    use crate::test_utils::setup_workspace;

    fn ignored_warnings(ned: &str, ini: &str) -> Vec<String> {
        let (db, _ned, ini_file) = setup_workspace(ned, ini);
        semantic_analysis(&db, ini_file)
            .iter()
            .filter(|d| d.message.contains("this value is ignored"))
            .map(|d| d.message.clone())
            .collect()
    }

    const APP_NED: &str = "\
simple App
{
    parameters:
        int fixedPort = 1000;
        int port = default(1000);
        int freePort;
}

module Host
{
    submodules:
        app: App;
}

network Net
{
    submodules:
        host: Host;
}
";

    #[test]
    fn fixed_declaration_is_reported() {
        let ini = "\
[General]
network = Net
**.app.fixedPort = 2000
";
        let warnings = ignored_warnings(APP_NED, ini);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("in App"), "{warnings:?}");
    }

    #[test]
    fn default_and_unassigned_declarations_are_not_reported() {
        let ini = "\
[General]
network = Net
**.app.port = 2000
**.app.freePort = 2000
";
        assert!(ignored_warnings(APP_NED, ini).is_empty());
    }

    #[test]
    fn assignment_in_derived_type_is_reported() {
        let ned = "\
simple App
{
    parameters:
        int port = default(1000);
}

simple FixedApp extends App
{
    parameters:
        port = 80;
}

network Net
{
    submodules:
        app: FixedApp;
}
";
        let ini = "\
[General]
network = Net
**.app.port = 2000
";
        let warnings = ignored_warnings(ned, ini);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("in FixedApp"), "{warnings:?}");
    }

    #[test]
    fn default_in_derived_type_is_not_reported() {
        let ned = "\
simple App
{
    parameters:
        int port = 1000;
}

simple OpenApp extends App
{
    parameters:
        port = default(80);
}

network Net
{
    submodules:
        app: OpenApp;
}
";
        let ini = "\
[General]
network = Net
**.app.port = 2000
";
        assert!(ignored_warnings(ned, ini).is_empty());
    }

    #[test]
    fn use_default_and_ask_are_not_reported() {
        let ned = "\
simple App
{
    parameters:
        int port = default(1000);
        int other = default(1000);
}

network Net
{
    submodules:
        app: App {
            port = default;
            other = ask;
        }
}
";
        let ini = "\
[General]
network = Net
Net.app.port = 2000
Net.app.other = 2000
";
        assert!(ignored_warnings(ned, ini).is_empty());
    }

    #[test]
    fn assignment_in_submodule_body_is_reported() {
        let ned = "\
simple App
{
    parameters:
        int port = default(1000);
}

network Net
{
    submodules:
        app: App {
            port = 80;
        }
        other: App;
}
";
        let ini = "\
[General]
network = Net
Net.app.port = 2000
Net.other.port = 2000
";
        let warnings = ignored_warnings(ned, ini);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("submodule 'app'"), "{warnings:?}");
    }

    #[test]
    fn pattern_assignment_in_enclosing_module_is_reported() {
        let ned = "\
simple App
{
    parameters:
        int port = default(1000);
}

module Host
{
    submodules:
        app: App;
}

network Net
{
    parameters:
        **.app.port = 80;
    submodules:
        host: Host;
}
";
        let ini = "\
[General]
network = Net
**.host.app.port = 2000
";
        let warnings = ignored_warnings(ned, ini);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("'**.app.port'"), "{warnings:?}");
    }

    #[test]
    fn path_assignment_in_submodule_body_is_reported() {
        let ned = "\
simple App
{
    parameters:
        int port = default(1000);
}

module Host
{
    submodules:
        app[2]: App;
}

network Net
{
    submodules:
        host: Host {
            app[*].port = 80;
        }
}
";
        let ini = "\
[General]
network = Net
**.host.app[*].port = 2000
";
        let warnings = ignored_warnings(ned, ini);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn index_specific_pattern_is_not_reported() {
        let ned = "\
simple App
{
    parameters:
        int port = default(1000);
}

module Host
{
    submodules:
        app[2]: App;
}

network Net
{
    submodules:
        host: Host {
            app[0].port = 80;
        }
}
";
        let ini = "\
[General]
network = Net
**.host.app[*].port = 2000
";
        assert!(ignored_warnings(ned, ini).is_empty());
    }

    #[test]
    fn fixed_type_value_wins_over_default_on_another_level() {
        let ned = "\
simple App
{
    parameters:
        int port = 1000;
}

network Net
{
    parameters:
        **.port = default(80);
    submodules:
        app: App;
}
";
        let ini = "\
[General]
network = Net
**.app.port = 2000
";
        let warnings = ignored_warnings(ned, ini);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("in App"), "{warnings:?}");
    }

    #[test]
    fn first_matching_pattern_counts() {
        let ned = "\
simple App
{
    parameters:
        int port = default(1000);
}

network Net
{
    parameters:
        **.port = default(80);
        app.port = 90;
    submodules:
        app: App;
}
";
        let ini = "\
[General]
network = Net
**.app.port = 2000
";
        assert!(ignored_warnings(ned, ini).is_empty());
    }

    #[test]
    fn fixed_value_in_replaced_type_is_not_reported() {
        // Host fixes app.port, but the INI file replaces Host with FreeHost
        let ned = "\
simple App
{
    parameters:
        int port = default(1000);
}

module Host
{
    parameters:
        app.port = 80;
    submodules:
        app: App;
}

module FreeHost
{
    submodules:
        app: App;
}

network Net
{
    submodules:
        host: Host;
}
";
        let ini = "\
[General]
network = Net
**.host.typename = \"FreeHost\"
**.host.app.port = 2000
";
        assert!(ignored_warnings(ned, ini).is_empty());
    }

    #[test]
    fn fixed_value_in_type_without_typename_override_is_reported() {
        let ned = "\
simple App
{
    parameters:
        int port = default(1000);
}

module Host
{
    parameters:
        app.port = 80;
    submodules:
        app: App;
}

network Net
{
    submodules:
        host: Host;
}
";
        let ini = "\
[General]
network = Net
**.host.app.port = 2000
";
        let warnings = ignored_warnings(ned, ini);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("'app.port' in Host"), "{warnings:?}");
    }

    #[test]
    fn only_reported_if_fixed_in_every_matched_module() {
        let ned = "\
simple App
{
    parameters:
        int port = default(1000);
}

network Net
{
    submodules:
        a: App {
            port = 80;
        }
        b: App;
}
";
        let ini = "\
[General]
network = Net
**.port = 2000
";
        assert!(ignored_warnings(ned, ini).is_empty());
    }

    #[test]
    fn parameter_patterns_are_not_reported() {
        let ini = "\
[General]
network = Net
**.app.fixed* = 2000
";
        assert!(ignored_warnings(APP_NED, ini).is_empty());
    }
}

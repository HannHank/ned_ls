use crate::db::Db;
use crate::hir::ned::submodules_hir::SubmoduleDefinition;
use crate::hir::ned::submodules_hir::SubmoduleHir;
use std::sync::Arc;
use std::sync::LazyLock;

use crate::{
    context::ItemContext,
    db::DefIndex,
    hir::{ItemHir, ned::ParamBlockItem},
};

pub struct ValidationScope<'a, 'db> {
    pub item: &'a ItemHir<'db>,
    pub context: Arc<ItemContext<'db>>,
    pub arc_defs: std::sync::Arc<Vec<DefIndex<'db>>>,
}

impl<'a, 'db> ValidationScope<'a, 'db> {
    pub fn new(
        item: &'a ItemHir<'db>,
        context: Arc<crate::context::ItemContext<'db>>,
        arc_defs: Arc<Vec<DefIndex<'db>>>,
    ) -> Self {
        Self {
            item,
            context,
            arc_defs,
        }
    }
}

// reduce parameters for resolve_deep_submodules
pub struct ParentScope<'a, 'db> {
    pub params: &'a [ParamBlockItem<'db>],
    pub context: &'a ItemContext<'db>,
}

// INFO: I am not fully happy with this approach → maybe I will pospone this to the check of the
// .ini files
static CONCRETE_TYPE_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#""([^"]+)""#).unwrap());

pub fn extract_all_concrete_types(text: &str) -> Vec<String> {
    let mut results = Vec::new();
    // A simple regex to extract all string literals
    for cap in CONCRETE_TYPE_RE.captures_iter(text) {
        results.push(cap[1].to_string());
    }
    results
}

pub fn match_glob(pattern: &str, target: &str) -> bool {
    // I tried to be clever with the glob matching and added some pre-checks to speed up the
    // matching (but I think there is still room for improvement)
    if pattern == target {
        return true;
    }

    if let Some(suffix) = pattern.strip_prefix("**.") {
        // no more wildcards
        if !suffix.contains('*') && !suffix.contains('?') {
            if target == suffix {
                return true;
            } else if target.ends_with(suffix) {
                let prefix_len = target.len() - suffix.len();
                // Ensure to not accidentally match "auto_tsn.app" to "tsn.app"
                if target.as_bytes()[prefix_len - 1] == b'.' {
                    return true;
                }
            }

            return false;
        }
    }

    // backtracking algorithm for glob matching
    const SEPARATOR: u8 = b'.';

    let (mut p_idx, mut t_idx) = (0, 0);
    let (mut star_idx, mut match_idx) = (None, 0);
    let p_bytes = pattern.as_bytes();
    let t_bytes = target.as_bytes();

    let (mut globstar_p_idx, mut globstar_t_idx) = (None, 0);

    // This is a standard algorithm for glob matching with support for '*' and '?'
    while t_idx < t_bytes.len() {
        if p_idx < p_bytes.len() && (p_bytes[p_idx] == b'?' || p_bytes[p_idx] == t_bytes[t_idx]) {
            p_idx += 1;
            t_idx += 1;
        } else if p_idx < p_bytes.len() && p_bytes[p_idx] == b'*' {
            if p_idx + 1 < p_bytes.len() && p_bytes[p_idx + 1] == b'*' {
                p_idx += 2; // need to skip both stars 
                globstar_p_idx = Some(p_idx);
                globstar_t_idx = t_idx;
                star_idx = None;
            } else {
                // Encountered single '*'
                p_idx += 1;
                star_idx = Some(p_idx);
                match_idx = t_idx;
            }
        } else if let Some(star) = star_idx {
            // check if the single star can match the current character
            if match_idx < t_bytes.len() && t_bytes[match_idx] == SEPARATOR {
                // single star cannot cross separator
                if let Some(g_star) = globstar_p_idx {
                    globstar_t_idx += 1;
                    t_idx = globstar_t_idx;
                    p_idx = g_star;
                    star_idx = None;
                } else {
                    return false;
                }
            } else {
                match_idx += 1;
                t_idx = match_idx;
                p_idx = star;
            }
        } else if let Some(g_star) = globstar_p_idx {
            // backtracking for double stars
            globstar_t_idx += 1;
            t_idx = globstar_t_idx;
            p_idx = g_star;
        } else {
            return false;
        }
    }

    // eat trailing stars in the pattern
    while p_idx < p_bytes.len() && p_bytes[p_idx] == b'*' {
        p_idx += 1;
    }

    p_idx == p_bytes.len()
}

/// Resolves the value of a parameter as a concrete type string, if possible.
fn resolve_param_value_as_type<'db>(
    db: &'db dyn crate::db::Db,
    parent: &ParentScope<'_, 'db>,
    param_name: &str,
) -> Option<String> {
    // Check the context entry (declaration with default value)
    if let Some(param_entry) = parent.context.parameters.get(param_name)
        && let ParamBlockItem::Param(p) = &param_entry.item
        && let Some(val) = p.value_expr(db)
        && let Some((resolved, true)) = extract_concrete_type(val.text(db))
        && !resolved.is_empty()
    {
        return Some(resolved);
    }

    // Scan raw params for direct assignments → these are skipped by merge_into_context
    // when they lack a type annotation, e.g. applType = "TraCIDemoRSU11p"
    for param in parent.params.iter().rev() {
        if let ParamBlockItem::Param(p) = param
            && !p.pattern(db)
            && p.name(db).text(db) == param_name
            && let Some(val) = p.value_expr(db)
            && let Some((resolved, true)) = extract_concrete_type(val.text(db))
            && !resolved.is_empty()
        {
            return Some(resolved);
        }
    }

    None
}

/// Extracts a concrete type from a string, returning the type and a boolean indicating if its known at compile time.
/// The bool is false if the type is only known at runtime
pub fn extract_concrete_type(text: &str) -> Option<(String, bool)> {
    let s = text.trim();

    if s.starts_with('"') && s.ends_with('"') {
        let inner = s.trim_matches('"');
        return Some((inner.to_string(), !inner.is_empty()));
    }

    Some((s.to_string(), !s.contains('(')))
}

pub fn get_submodule_actual_type<'db>(
    db: &'db dyn crate::db::Db,
    parent: &ParentScope<'_, 'db>,
    sub_name: &str,
    submodule: &SubmoduleHir<'db>,
) -> Option<(String, bool)> {
    // Check if typename is assigned in the current modules parameters
    for param in parent.params {
        if let ParamBlockItem::Param(p) = param
            && *p.pattern(db)
        {
            let segments = p.segments(db);
            if let Some(target) = p.target_param(db) {
                let target_param_name = target.text(db);

                let sub_pattern_str = segments
                    .iter()
                    .map(|s| s.name(db).text(db).as_str())
                    .collect::<Vec<_>>()
                    .join(".");

                if sub_pattern_str == sub_name
                    && target_param_name == "typename"
                    && let Some(val) = p.value_expr(db)
                    && let Some((extracted, true)) = extract_concrete_type(val.text(db))
                {
                    return Some((extracted, true));
                }
            }
        }
    }

    // Check inherited/context parameters for sub_name.typename
    let target_name = format!("{}.typename", sub_name);
    if let Some(ctx_param) = parent.context.parameters.get(&target_name)
        && let ParamBlockItem::Param(p) = &ctx_param.item
        && let Some(val) = p.value_expr(db)
        && let Some((extracted, true)) = extract_concrete_type(val.text(db))
    {
        return Some((extracted, true));
    }

    // Scan raw params for the same pattern (merge_into_context skips assignments
    // without type annotations, so the context check above may miss them)
    for param in parent.params {
        if let ParamBlockItem::Param(p) = param
            && p.name(db).text(db) == &target_name
            && let Some(val) = p.value_expr(db)
            && let Some((extracted, true)) = extract_concrete_type(val.text(db))
        {
            return Some((extracted, true));
        }
    }

    // Otherwise is must be defined inside the submodule block
    match &submodule.definition {
        SubmoduleDefinition::Concrete { type_name, .. } => Some((type_name.clone(), true)),
        SubmoduleDefinition::Parametric {
            interface_name,
            like_expr,
            ..
        } => {
            if let Some(expr) = like_expr
                && let Some((extracted_name, is_concrete)) = extract_concrete_type(expr)
                && is_concrete
            {
                if parent.context.parameters.contains_key(&extracted_name) {
                    if let Some(resolved) = resolve_param_value_as_type(db, parent, &extracted_name)
                    {
                        return Some((resolved, true));
                    }
                } else {
                    return Some((extracted_name, true));
                }
            }
            Some((interface_name.clone(), false))
        }
    }
}

// Is doing essentially the same as get_type_structure but only for one level which is important for
// completion in the connection section → Might change in the future
pub fn resolve_submodule_context<'db>(
    db: &'db dyn Db,
    containing_module: &ItemHir<'db>,
    base_context: &ItemContext<'db>,
    sub_name: &str,
    sub_mod: &SubmoduleHir<'db>,
    visible_defs: Arc<Vec<DefIndex<'db>>>,
) -> Option<Arc<ItemContext<'db>>> {
    let parent_scope = ParentScope {
        params: containing_module.parameters(db),
        context: base_context,
    };

    let (actual_type_str, _) =
        get_submodule_actual_type(db, &parent_scope, sub_name, sub_mod).unwrap_or_default();

    let clean_type_str = actual_type_str.trim_matches(|c| c == '"' || c == '\'');

    if let Some(local_type) = base_context.types.get(clean_type_str) {
        Some(crate::context::get_item_context(db, local_type.item, visible_defs).clone())
    } else {
        crate::context::get_submodule_context_from_type(db, clean_type_str, visible_defs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_matching() {
        assert!(match_glob("*.ned", "network.ned"));
        assert!(match_glob("**.*.ned", "src.network.ned"));
        assert!(match_glob("**.*.ned", "src.subdir.network.ned"));
        assert!(match_glob("src.**.*.ned", "src.subdir.network.ned"));
        assert!(!match_glob("src.*.ned", "src.subdir.network.ned"));
        assert!(match_glob("**.network.ned", "src.subdir.network.ned"));
        assert!(!match_glob("**.network.ned", "src.subdir.other.ned"));
    }
}

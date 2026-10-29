use tower_lsp::lsp_types::{
    CompletionItem, CompletionTextEdit, Documentation, MarkupContent, MarkupKind, Position, Range,
    TextEdit,
};

/// Reference to the NED type or parameter behind a completion item
/// I read the documentation lazily in order to avoid loading all documentation for all suggestions
/// immediately
#[derive(serde::Serialize, serde::Deserialize)]
struct DocRef {
    #[serde(rename = "type")]
    fqn: String,
    /// Set for a parameter declared in that type
    #[serde(default, skip_serializing_if = "Option::is_none")]
    param: Option<String>,
}

/// data for a completion item that stands for the type def
pub fn type_doc_data(
    db: &dyn crate::db::Db,
    def: crate::db::DefIndex,
) -> Option<serde_json::Value> {
    serde_json::to_value(DocRef {
        fqn: def.fqn(db).text(db).clone(),
        param: None,
    })
    .ok()
}

/// data for a completion item param
pub fn param_doc_data(
    db: &dyn crate::db::Db,
    declared_in: crate::db::DefIndex,
    param: &str,
) -> Option<serde_json::Value> {
    serde_json::to_value(DocRef {
        fqn: declared_in.fqn(db).text(db).clone(),
        param: Some(param.to_string()),
    })
    .ok()
}

fn def_by_fqn<'db>(db: &'db dyn crate::db::Db, fqn: &str) -> Option<crate::db::DefIndex<'db>> {
    if fqn.contains('.') {
        let pattern = crate::hir::ned::import_hir::ImportPattern::new(db, fqn.to_string());
        crate::db::package_tree::resolve_import(db, pattern)
            .as_ref()
            .and_then(|defs| defs.first().copied())
    } else {
        *crate::db::package_tree::resolve_global_type(
            db,
            crate::hir::Word::new(db, fqn.to_string()),
        )
    }
}

/// Adds the documentation to a completion item created with type_doc_data or param_doc_data
/// Other items are returned unchanged.
pub fn resolve_item(db: &dyn crate::db::Db, mut item: CompletionItem) -> CompletionItem {
    if item.documentation.is_some() {
        return item;
    }
    let Some(doc_ref) = item
        .data
        .clone()
        .and_then(|data| serde_json::from_value::<DocRef>(data).ok())
    else {
        return item;
    };
    let Some(def) = def_by_fqn(db, &doc_ref.fqn) else {
        return item;
    };

    let uri = def.uri(db);
    let name = def.simple_name(db).text(db);
    let doc = match &doc_ref.param {
        Some(param) => crate::utils::documentation_for_parameter(db, uri, name, param),
        None => crate::utils::documentation_for_type(db, uri, name),
    };

    // eprintln!("resolve_item: {:?} -> {:?}", doc_ref, doc);
    if let Some(doc) = doc {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc,
        }));
    }
    item
}

/// Range from the start of the token currently being typed to the cursor.
/// The token starts after the last delimiter
pub fn replacement_range(text_before_cursor: &str, delims: &[char], cursor: Position) -> Range {
    let mut start = 0;
    for (i, ch) in text_before_cursor.chars().enumerate() {
        if delims.contains(&ch) {
            start = i + 1;
        }
    }
    for ch in text_before_cursor.chars().skip(start) {
        if ch.is_whitespace() {
            start += 1;
        } else {
            break;
        }
    }

    // LSP columns are utf-16 code units
    let utf16_col: usize = text_before_cursor
        .chars()
        .take(start)
        .map(|c| c.len_utf16())
        .sum();

    Range {
        start: Position {
            line: cursor.line,
            character: utf16_col as u32,
        },
        end: cursor,
    }
}

/// Attaches the replacement range to all items so clients replace the typed
/// token instead of relying on prefix filtering.
//
// Most of the time vscode is able to figure it out, but I had some edge cases where the suggestion
// was applied in full lenght and the token was duplicated. In addition maybe other clients have no
// prefix filtering at all
pub fn attach_range(items: Vec<CompletionItem>, range: Range) -> Vec<CompletionItem> {
    items
        .into_iter()
        .map(|mut item| {
            let new_text = item
                .insert_text
                .clone()
                .unwrap_or_else(|| item.label.clone());
            item.text_edit = Some(CompletionTextEdit::Edit(TextEdit { range, new_text }));
            item
        })
        .collect()
}

#[cfg(test)]
mod doc_tests {
    use super::*;
    use crate::test_utils::setup_ned;

    const NED: &str = "\
package demo;

// A host that runs applications.
module Host
{
    parameters:
        int numApps = default(0); // number of applications
}

module BigHost extends Host
{
}
";

    fn documentation(item: &CompletionItem) -> Option<&str> {
        match &item.documentation {
            Some(Documentation::MarkupContent(content)) => Some(&content.value),
            _ => None,
        }
    }

    fn item_with(data: Option<serde_json::Value>) -> CompletionItem {
        CompletionItem {
            label: "item".to_string(),
            data,
            ..Default::default()
        }
    }

    #[test]
    fn global_type_suggestion_resolves_to_its_documentation() {
        let (db, _file) = setup_ned(NED);
        let suggestions = crate::completion::ned::get_global_suggestions(&db, &[], None);
        let host = suggestions.iter().find(|s| s.name == "Host").unwrap();

        let resolved = resolve_item(&db, item_with(host.doc.clone()));
        assert_eq!(
            documentation(&resolved),
            Some("A host that runs applications.")
        );
    }

    #[test]
    fn inherited_parameter_resolves_to_its_declaration() {
        let (db, file) = setup_ned(NED);
        let hir = crate::hir::ned::lower_to_hir(&db, file);
        let big_host = hir
            .items(&db)
            .iter()
            .find(|item| item.name(&db).text(&db) == "BigHost")
            .unwrap();
        let ctx = crate::context::get_item_context(
            &db,
            *big_host,
            crate::db::visible_definitions(&db, file).clone(),
        );
        // declared in Host, not in BigHost
        let entry = ctx.parameters.get("numApps").unwrap();

        let data = param_doc_data(&db, entry.origin, "numApps");
        let resolved = resolve_item(&db, item_with(data));
        assert_eq!(documentation(&resolved), Some("number of applications"));
    }

    #[test]
    fn unknown_type_leaves_item_unchanged() {
        let (db, _file) = setup_ned(NED);
        let data = serde_json::json!({ "type": "demo.Missing" });
        let item = item_with(Some(data));
        assert_eq!(resolve_item(&db, item.clone()), item);
    }
}

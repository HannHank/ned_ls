mod completion_engine;
mod connections;
mod gates;
mod import;
mod parameters;

pub mod package;
use crate::{
    SharedState,
    db::{self, Db, DefKind},
    hir::ned::section_order::SectionKind,
    utils,
};

use completion_engine::CompletionEngine;
use connections::connections_completions;
use gates::gates_completions;
use parameters::parameters_completions;

use ropey::Rope;
use std::{collections::HashMap, sync::Arc};
use tower_lsp::lsp_types::{CompletionItemKind, Range, TextDocumentPositionParams};
use tree_sitter::Tree;
use tree_sitter_ned::language as ned_language;

pub enum CompletionItemResponse {
    Import(String, Vec<String>, Range),
    Keywords(Vec<&'static str>, Range),
    Snippet(String, String, Range),
    Parameters(Vec<GenericSuggestion>, Range),
    Gates(Vec<String>, Range),
    Submodules(Vec<String>, Range),
    GlobalSuggestions {
        kind: CompletionItemKind,
        suggestions: Vec<GlobalSuggestion>,
        range: Range,
        // line for auto-import
        import_insert_line: u32,
    },
    Package(String, Range),
}

// TODO: refactor → maybe it is better to directly return the CompletionItems. At the beginning this
// was not the case because I wanted to keep the completion engine decoupled from the LSP types but
// now I am moving more towards reusable code and it feels like just creating an indirection
#[derive(Debug, Clone)]
pub struct GenericSuggestion {
    pub name: String,
    pub detail: Option<String>,
    /// Reference for the documentation → becomes the data field of the completion item
    pub doc: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct GlobalSuggestion {
    pub name: String,
    pub fqn: String,
    pub needs_import: bool,
    /// Reference for the documentation → becomes the data of the completion item
    pub doc: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextType {
    None,
    StringLiteral,
    SimpleModule,
    CompoundModule,
    Gates,
    Gate,
    SubmoduleGates, // Prefix Submodule not really needed, since I am now fetching the ctx path
    Parameters,
    Submodules,
    Submodule,
    LocalTypes, // Type section inside modules → do not get confused with the Type from submodules
    Type,
    LikeType,
    LikeExpr,
    SubmoduleParameters,
    Connections,
    Connection,
    ConnectionSrc,
    ConnectionDst,
    PackageLine,
    ImportLine,
    ExtendsLine,
    LikeLine,
    ModuleInterface,
    Network,
    Channel,
    NedFile,
}

// pub struct ContextFrame {
//     pub ty: ContextType,
//     pub byte_range: (usize, usize),
// }

pub type ContextPath = Vec<ContextType>;

type CompletionResult = HashMap<String, CompletionItemResponse>;

pub async fn completion(
    pos: &TextDocumentPositionParams,
    backend: Arc<SharedState>,
) -> Result<CompletionResult, Box<dyn std::error::Error + Sync + Send>> {
    // Extract
    let uri = &pos.text_document.uri;
    let position = pos.position;

    // INFO: This is important to avoid race conditions with the incremental parser that is also
    // accessing these maps
    let (rope, ast, ned_file) = {
        let db = backend.db.lock().await;
        let ned_file = match db.get_source_file_input(uri) {
            Some(f) => f,
            None => return Ok(HashMap::new()),
        };
        let ast = match db.get_ast_for_file(&ned_file) {
            Some(t) => t,
            None => return Ok(HashMap::new()),
        };
        (ned_file.rope(&*db).clone(), ast, ned_file)
    };

    // backend
    //     .client
    //     .log_message(MessageType::INFO, "From completion context: ")
    //     .await;

    let char_offset = utils::position_to_char(&rope, position);

    let byte_offset = rope.char_to_byte(char_offset); // This should no longer panic if position_to_char is correct

    let mut completions: HashMap<String, CompletionItemResponse> = HashMap::new();

    // let line_idx = rope.char_to_line(char_offset);
    // let line_start_char = rope.line_to_char(line_idx);

    // let text_before_cursor = rope.slice(line_start_char..char_offset).to_string();

    // let is_blank_line = text_before_cursor.trim().is_empty();

    // INFO: have not decided if I want to keep the speculative parsing. Sometimes it does
    // more harm than good
    // let ast = if is_blank_line {
    //     ast
    // } else {
    //     match speculative_parsing(&rope, &ast, byte_offset) {
    //         (Some(t), _) => t,
    //         _ => ast, // Safe fallback
    //     }
    // };
    let (ast, _) = match speculative_parsing(&rope, &ast, byte_offset) {
        (Some(t), Some(r)) => (t, r),
        _ => {
            return Ok(completions);
        }
    };

    let db = {
        let db_guard = backend.db.lock().await;
        db_guard.clone()
    };

    let engine = CompletionEngine {
        db: &db,
        rope: &rope,
        ast: &ast,
        uri,
        position,
        byte_offset,
        char_offset,
    };

    let mut ctx_path = engine.get_current_context();

    // token being typed and the range it occupies → replaced by accepted suggestions
    let (partial_word, token_range) = engine.get_replacement_range();

    log::debug!("current ctx: {:?}", ctx_path);

    let ignored_contexts = [ContextType::StringLiteral];

    //  I need the string_literal inside the like_expr to check if I sould suggest a module or a
    //  parameter. However this is still not very robust if the Ast is broken.
    if let Some(first) = ctx_path.first()
        && ignored_contexts.contains(first)
    {
        let temp = ctx_path.remove(0);
        ctx_path.push(temp);
    }

    match ctx_path.first() {
        Some(ContextType::ImportLine) => {
            let suggested_imports = import::suggest_imports(engine.db, &partial_word);
            completions.insert(
                "import".to_string(),
                CompletionItemResponse::Import(
                    partial_word.clone(),
                    suggested_imports,
                    token_range,
                ),
            );
        }

        Some(ContextType::ExtendsLine) => {
            let visible_defs = crate::db::visible_definitions(engine.db, ned_file);

            let suggestions = get_global_suggestions(engine.db, visible_defs, None);
            completions.insert(
                "Extends".to_string(),
                CompletionItemResponse::GlobalSuggestions {
                    kind: CompletionItemKind::CLASS,
                    suggestions,
                    range: token_range,
                    import_insert_line: import_insert_line(engine.db, ned_file, engine.rope),
                },
            );
        }

        Some(ContextType::LikeLine) => {
            let visible_defs = crate::db::visible_definitions(engine.db, ned_file);

            let suggestions =
                get_global_suggestions(engine.db, visible_defs, Some(&[DefKind::ModuleInterface]));
            completions.insert(
                "Like".to_string(),
                CompletionItemResponse::GlobalSuggestions {
                    kind: CompletionItemKind::INTERFACE,
                    suggestions,
                    range: token_range,
                    import_insert_line: import_insert_line(engine.db, ned_file, engine.rope),
                },
            );
        }

        Some(ContextType::PackageLine) => {
            completions.insert(
                "package_example".to_string(),
                CompletionItemResponse::Package(
                    package::resolve_package_name(&engine),
                    token_range,
                ), // TODO:
                   // add error type to CompletionItemResponse with text in order to avoid making this
                   // function async
            );
        }

        Some(ContextType::Gates) => {
            let keywords = vec!["input", "output", "connections"];
            completions.insert(
                "Gate".to_string(),
                CompletionItemResponse::Keywords(keywords, token_range),
            );

            gates_completions(&engine, &mut completions);
        }

        #[allow(clippy::collapsible_match)]
        Some(ContextType::Gate) => {
            // Gate is used in the gate and in the connections section → maybe I should rename it to
            // avoid this
            if ctx_path
                .iter()
                .any(|c| matches!(c, ContextType::Connections | ContextType::ConnectionSrc))
            {
                connections_completions(&engine, &mut completions);
            }
        }

        Some(ContextType::SubmoduleGates) => {
            gates_completions(&engine, &mut completions);
        }

        Some(ContextType::Parameters) => {
            // Insert Keywords
            let keywords = vec!["volatile", "double", "int", "@unit", "@display"];
            completions.insert(
                "Keywords".to_string(),
                CompletionItemResponse::Keywords(keywords, token_range),
            );

            // Context aware parameter suggestions
            parameters_completions(&engine, &mut completions, true)
        }

        Some(ContextType::Submodules) => {} // INFO: Not sure what to suggest here

        Some(ContextType::SubmoduleParameters) => {
            // Insert Keywords
            let keywords = vec!["@display"]; // TODO: need
            // to check which keywords are allowed
            completions.insert(
                "Keywords".to_string(),
                CompletionItemResponse::Keywords(keywords, token_range),
            );
            // Context aware parameter suggestions
            parameters_completions(&engine, &mut completions, true)
        }

        Some(ContextType::Type) => {
            // Suggest all visible modules first and then all fqns like for imports
            let visible_defs = crate::db::visible_definitions(engine.db, ned_file);

            let suggestions = get_global_suggestions(
                engine.db,
                visible_defs,
                Some(&[
                    DefKind::SimpleModule,
                    DefKind::CompoundModule,
                    DefKind::Network,
                ]),
            );

            completions.insert(
                "SubmoduleType".to_string(),
                CompletionItemResponse::GlobalSuggestions {
                    kind: CompletionItemKind::CLASS,
                    suggestions,
                    range: token_range,
                    import_insert_line: import_insert_line(engine.db, ned_file, engine.rope),
                },
            );

            // Insert Snippet for <default("")> like
            completions.insert(
                "SubmoduleTypeSnippet".to_string(),
                CompletionItemResponse::Snippet(
                    "default()".to_string(),
                    "<default(\"${1:Module}\")> like ${2:Interface} {\n\n}".to_string(),
                    token_range,
                ),
            );
        }

        Some(ContextType::LikeType) => {
            // Suggest all visible modules first and then all fqns like for imports
            let visible_defs = crate::db::visible_definitions(engine.db, ned_file);

            let suggestions =
                get_global_suggestions(engine.db, visible_defs, Some(&[DefKind::ModuleInterface]));

            completions.insert(
                "SubmoduleType".to_string(),
                CompletionItemResponse::GlobalSuggestions {
                    kind: CompletionItemKind::INTERFACE,
                    suggestions,
                    range: token_range,
                    import_insert_line: import_insert_line(engine.db, ned_file, engine.rope),
                },
            );
        }

        Some(ContextType::LikeExpr) => {
            if ctx_path.contains(&ContextType::StringLiteral) {
                // A little bit weird but inside like_expr modules can be used without beeing imported
                // or without the usage of their fqn.
                //
                // This means we can just return all simple names for modules in the workspace
                let indexes = db::workspace_indexes(engine.db);

                let suggestions: Vec<GlobalSuggestion> = indexes
                    .suffix_index
                    .iter()
                    .filter_map(|(name, defs)| {
                        defs.iter()
                            .find(|def| {
                                matches!(
                                    def.kind(engine.db),
                                    DefKind::SimpleModule | DefKind::CompoundModule
                                )
                            })
                            .map(|def| GlobalSuggestion {
                                name: name.clone(),
                                fqn: def.fqn(engine.db).text(engine.db).clone(),
                                needs_import: false, // No import needed for like expressions
                                doc: crate::completion::shared::type_doc_data(engine.db, *def),
                            })
                    })
                    .collect();

                completions.insert(
                    "LikeExpr".to_string(),
                    CompletionItemResponse::GlobalSuggestions {
                        kind: CompletionItemKind::CLASS,
                        suggestions,
                        range: token_range,
                        // needs_import is always false inside like-exprs
                        import_insert_line: 0,
                    },
                );
            } else {
                // We are in the default but not in a string_literal → provide parameters
                parameters_completions(&engine, &mut completions, false);
            }
        }

        Some(ContextType::Connections) | Some(ContextType::ConnectionSrc) => {
            connections_completions(&engine, &mut completions)
        }

        Some(ContextType::NedFile) => {
            let keywords = vec![
                "import",
                "package",
                "simple",
                "moduleinterface",
                "network",
                "module",
                "channel",
            ];

            completions.insert(
                "keywords".to_string(),
                CompletionItemResponse::Keywords(keywords, token_range),
            );

            // // check last keyword
            // let prev_token = engine.get_previous_token();
            //
            // if prev_token == "extends" {
            //     completions.insert(
            //         "ModuleKeywords".to_string(),
            //         CompletionItemResponse::Keywords(vec![
            //             "parameters",
            //             "gates",
            //             "submodules",
            //             "connections",
            //         ]),
            //     );
            // }
        }
        _ => { /* For now */ }
    }

    // I want to provide the sections snippets based on what sections are really allowed as opposed
    // to the opp_ide
    let is_in_module = ctx_path.contains(&ContextType::CompoundModule)
        || ctx_path.contains(&ContextType::SimpleModule)
        // || ctx_path.contains(&ContextType::NedFile)
        || ctx_path.contains(&ContextType::Network)
        || ctx_path.contains(&ContextType::Channel)
        || ctx_path.contains(&ContextType::ModuleInterface);

    let is_in_submodule_braces = ctx_path.contains(&ContextType::SubmoduleParameters)
        || ctx_path.contains(&ContextType::SubmoduleGates)
        || ctx_path.contains(&ContextType::Submodule);

    if is_in_module && !is_in_submodule_braces {
        let active_module_ctx = ctx_path
            .iter()
            .find(|c| {
                matches!(
                    c,
                    ContextType::SimpleModule
                        | ContextType::ModuleInterface
                        | ContextType::Channel
                        | ContextType::CompoundModule
                        // | ContextType::NedFile
                        | ContextType::Network
                )
            })
            .unwrap_or(&ContextType::CompoundModule); // Safe default → in the worst case provides all sections without filtering any out

        // find highest section before cursor
        let mut max_rank_seen = -1i16;

        let mut current_node = engine
            .ast
            .root_node()
            .descendant_for_byte_range(engine.byte_offset, engine.byte_offset)
            .unwrap_or(engine.ast.root_node());

        while !["module", "simple", "network", "moduleinterface", "channel"]
            .contains(&current_node.kind())
        {
            if let Some(parent) = current_node.parent() {
                current_node = parent;
            } else {
                break;
            }
        }

        // check which sections already passed
        let mut cursor = current_node.walk();
        for child in current_node.children(&mut cursor) {
            if child.end_byte() < engine.byte_offset
                && let Ok(kind) = child.kind().parse::<SectionKind>()
                && let Some(rank) = kind.order_rank()
                && (rank as i16) > max_rank_seen
            {
                max_rank_seen = rank as i16;
            }
        }

        for (kind, snippet) in SectionKind::ALL {
            if !kind.is_allowed_in(active_module_ctx) {
                continue;
            }

            // Check for order
            if let Some(rank) = kind.order_rank()
                && (rank as i16) <= max_rank_seen
            {
                continue;
            }

            let label = kind.to_string();
            completions
                .entry(format!("section_{}", label))
                .or_insert_with(|| {
                    CompletionItemResponse::Snippet(label.clone(), snippet.to_string(), token_range)
                });
        }

        completions.insert(
            "class_keyword".to_string(),
            CompletionItemResponse::Keywords(vec!["@class"], token_range),
        );
    }

    Ok(completions)
}

/// Performs speculative parsing by inserting a fake identifier at the cursor position and re-parsing the AST.
pub fn speculative_parsing(
    rope: &Rope,
    tree: &Tree,
    offset: usize,
) -> (Option<Tree>, Option<Rope>) {
    let char_offset = match rope.try_byte_to_char(offset) {
        Ok(c) => c,
        Err(_) => return (None, None),
    };

    let mut temp_rope = rope.clone();

    // INFO: I had different versions taking the current context into account but I decided to
    // implemented the fallback context resolution instead, making the speculative parsing easier
    const FAKE_IDENT: &str = "_COMPLETION_TOKEN_;";
    temp_rope.insert(char_offset, FAKE_IDENT);

    let fake_ident_len_bytes = FAKE_IDENT.len();
    let new_end_byte = offset + fake_ident_len_bytes;
    let start_position = utils::byte_to_point(rope, offset);
    let new_end_position = utils::byte_to_point(&temp_rope, new_end_byte);

    let input_edit = tree_sitter::InputEdit {
        start_byte: offset,
        old_end_byte: offset,
        new_end_byte,
        start_position,
        old_end_position: start_position,
        new_end_position,
    };

    let mut edited_tree = tree.clone();
    edited_tree.edit(&input_edit);

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ned_language()).is_err() {
        return (None, None);
    };

    let new_tree = parser.parse_with_options(
        &mut |byte_offset, _position| {
            if byte_offset >= temp_rope.len_bytes() {
                return "";
            }
            let (chunk, chunk_byte_idx, _, _) = temp_rope.chunk_at_byte(byte_offset);
            &chunk[byte_offset - chunk_byte_idx..]
        },
        Some(&edited_tree),
        None,
    );

    (new_tree, Some(temp_rope))
}

/// Extracts the target submodule name if the user typed a dot.
pub fn get_dot_target_name(last_token: &str) -> Option<&str> {
    if let Some(dot_idx) = last_token.rfind('.') {
        let prefix = &last_token[..dot_idx];
        Some(prefix.split('[').next().unwrap_or(prefix).trim())
    } else {
        None
    }
}

/// Line where an auto-import should be inserted:
/// after the last import, otherwise after the package line, otherwise at the top.
fn import_insert_line(
    db: &dyn crate::db::Db,
    ned_file: crate::hir::SourceFile,
    rope: &Rope,
) -> u32 {
    let hir = crate::hir::ned::lower_to_hir(db, ned_file);

    let line = if let Some(last_import) = hir.imports(db).last() {
        rope.byte_to_line(last_import.span.end) + 1
    } else if let Some(package) = hir.package(db) {
        rope.byte_to_line(package.span.end) + 1
    } else {
        0
    };
    line as u32
}

pub fn get_global_suggestions<'db>(
    db: &'db dyn crate::db::Db,
    visible_defs: &[crate::db::DefIndex<'db>],
    allowed_kinds: Option<&[DefKind]>, // Pass None for all, or specific kinds like [DefKind::ModuleInterface]
) -> Vec<GlobalSuggestion> {
    let mut suggestions = Vec::new();

    // Create a fast lookup set of already visible FQNs
    let visible_fqns: std::collections::HashSet<String> = visible_defs
        .iter()
        .map(|def| def.fqn(db).text(db).to_string())
        .collect();

    let indexes = crate::db::package_tree::workspace_indexes(db);

    // Iterate over all known global definitions
    for (simple_name, def_list) in indexes.suffix_index.iter() {
        for def in def_list {
            let kind = def.kind(db);
            let fqn = def.fqn(db).text(db).clone();

            // Filter by kind if required
            if let Some(allowed) = allowed_kinds
                && !allowed.contains(kind)
            {
                continue;
            }

            // do not auto-import if visible
            let needs_import = !visible_fqns.contains(&fqn);

            suggestions.push(GlobalSuggestion {
                name: simple_name.clone(),
                fqn,
                needs_import,
                doc: crate::completion::shared::type_doc_data(db, *def),
            });
        }
    }

    suggestions
}

#[cfg(test)]
mod tests {
    use ropey::Rope;
    use tower_lsp::lsp_types::Position;

    /// This is a simplified version of the logic in CompletionEngine::get_last_token, designed to be easily testable in isolation.
    pub fn mock_get_last_token(rope: &Rope, mut char_offset: usize) -> String {
        let mut skipped_space = false;
        let mut hit_newline = false;

        while char_offset > 0 {
            let ch = rope.char(char_offset - 1);
            if ch == '\n' || ch == '\r' {
                hit_newline = true;
                skipped_space = false;
                char_offset -= 1;
            } else if ch.is_whitespace() {
                skipped_space = true;
                char_offset -= 1;
            } else {
                break;
            }
        }

        if !hit_newline && skipped_space {
            return "".to_string();
        }

        let end = char_offset;
        while char_offset > 0 && !rope.char(char_offset - 1).is_whitespace() {
            char_offset -= 1;
        }

        let token = rope.slice(char_offset..end).to_string();
        if hit_newline && !token.ends_with('.') {
            return "".to_string();
        }
        token
    }

    #[test]
    fn test_last_token_and_replacement_range() {
        let text = "\
import inet.common.Hookc; // comment
import my.module // incomplete
import another.module
";
        let rope = Rope::from_str(text);

        let simulate_cursor = |target_word: &str| {
            let byte_offset = text.find(target_word).unwrap() + target_word.len();
            let char_offset = rope.byte_to_char(byte_offset);

            // Calculate standard LSP 2D Position → very important
            let line_idx = rope.char_to_line(char_offset);
            let char_start_of_line = rope.line_to_char(line_idx);
            let position = Position {
                line: line_idx as u32,
                character: (char_offset - char_start_of_line) as u32,
            };

            (char_offset, position)
        };

        let (char_offset, position) = simulate_cursor("Hookc");

        let partial_word = mock_get_last_token(&rope, char_offset);
        assert_eq!(partial_word, "inet.common.Hookc");

        let char_length = partial_word.chars().count() as u32;
        let start_pos = Position {
            line: position.line,
            character: position.character.saturating_sub(char_length),
        };

        // import inet.common.Hookc -> The word starts at character index 7
        assert_eq!(start_pos.character, 7);

        let (char_offset, position) = simulate_cursor("my.module");

        let partial_word = mock_get_last_token(&rope, char_offset);
        assert_eq!(partial_word, "my.module");

        let char_length = partial_word.chars().count() as u32;
        let start_pos = Position {
            line: position.line,
            character: position.character.saturating_sub(char_length),
        };

        assert_eq!(start_pos.character, 7);
    }
}

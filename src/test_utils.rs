use ropey::Rope;
use tower_lsp::lsp_types::{Position, Url};

use crate::db::{Db, RootLspDatabase};
use crate::hir;

fn add_file(
    db: &mut RootLspDatabase,
    uri: &str,
    text: &str,
    language: &tree_sitter::Language,
) -> hir::SourceFile {
    let uri = Url::parse(uri).expect("invalid test uri");
    let file = hir::SourceFile::new(&*db, Rope::from_str(text), uri.clone(), 0);
    db.set_ned_file_input(uri, file);

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(language)
        .expect("Failed to set language");
    let tree = parser.parse(text, None).expect("Failed to parse test text");
    db.set_ast_for_file(file, tree);

    file
}

/// Registers a parsed NED file in the database.
pub fn add_ned_file(db: &mut RootLspDatabase, uri: &str, text: &str) -> hir::SourceFile {
    add_file(db, uri, text, &tree_sitter_ned::language())
}

/// Registers a parsed INI file in the database.
pub fn add_ini_file(db: &mut RootLspDatabase, uri: &str, text: &str) -> hir::SourceFile {
    add_file(db, uri, text, &tree_sitter_omnetpp_ini::LANGUAGE.into())
}

/// A database with a single NED file
pub fn setup_ned(text: &str) -> (RootLspDatabase, hir::SourceFile) {
    let mut db = RootLspDatabase::default();
    let file = add_ned_file(&mut db, "file:///test.ned", text);
    (db, file)
}

/// A database with a single INI file
pub fn setup_ini(text: &str) -> (RootLspDatabase, hir::SourceFile) {
    let mut db = RootLspDatabase::default();
    let file = add_ini_file(&mut db, "file:///test.ini", text);
    (db, file)
}

/// A database with a NED and an INI file → cross file resolution
pub fn setup_workspace(
    ned_text: &str,
    ini_text: &str,
) -> (RootLspDatabase, hir::SourceFile, hir::SourceFile) {
    let mut db = RootLspDatabase::default();
    let ned = add_ned_file(&mut db, "file:///test.ned", ned_text);
    let ini = add_ini_file(&mut db, "file:///test.ini", ini_text);
    (db, ned, ini)
}

/// Byte offset of the middle of word, searched after anchor → tree-sitter resolves the node the word belongs to
pub fn word_offset(text: &str, anchor: &str, word: &str) -> usize {
    let anchor_pos = text
        .find(anchor)
        .unwrap_or_else(|| panic!("anchor {anchor:?} not found"));
    let rest = &text[anchor_pos + anchor.len()..];
    let word_pos = rest
        .find(word)
        .unwrap_or_else(|| panic!("word {word:?} not found after anchor"));
    anchor_pos + anchor.len() + word_pos + word.len() / 2
}

/// LSP position of the middle of word → searched after anchor.
pub fn pos_of(text: &str, anchor: &str, word: &str) -> Position {
    let offset = word_offset(text, anchor, word);
    let before = &text[..offset];
    let line = before.matches('\n').count() as u32;
    let character = (offset - before.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
    Position { line, character }
}

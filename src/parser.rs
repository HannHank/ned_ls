// Abstraction to hide the complexity of parsing ned and msg files using tree-sitter

use tower_lsp::lsp_types::Url;
use tree_sitter::Tree;

pub struct Parser {
    ned_parser: tree_sitter::Parser,
    ini_parser: tree_sitter::Parser,
}

impl Parser {
    pub fn new() -> Self {
        let mut ned_parser = tree_sitter::Parser::new();
        ned_parser
            .set_language(&tree_sitter_ned::language())
            .expect("Error loading NED grammar");

        let mut ini_parser = tree_sitter::Parser::new();
        ini_parser
            .set_language(&tree_sitter_omnetpp_ini::LANGUAGE.into())
            .expect("Error loading MSG grammar");

        Parser {
            ned_parser,
            ini_parser,
        }
    }

    pub fn parse(
        &mut self,
        url: &Url,
        content: &str,
        tree_opt: Option<&Tree>,
    ) -> Option<tree_sitter::Tree> {
        let ext = url.path().rsplit('.').next().unwrap_or_default();
        match ext {
            "ned" => self.ned_parser.parse(content, tree_opt),
            "ini" => self.ini_parser.parse(content, tree_opt),
            _ => None,
        }
    }

    pub fn parse_with_options<T: AsRef<[u8]>, F: FnMut(usize, tree_sitter::Point) -> T>(
        &mut self,
        uri: &Url,
        callback: &mut F,
        old_tree: Option<&Tree>,
    ) -> Option<Tree> {
        let ext = uri.path().rsplit('.').next().unwrap_or_default();
        match ext {
            "ned" => self.ned_parser.parse_with_options(callback, old_tree, None),
            "ini" => self.ini_parser.parse_with_options(callback, old_tree, None),
            _ => None,
        }
    }
}

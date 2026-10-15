pub mod channel_hir;
pub mod compound_module_hir;
pub mod connections_hir;
pub mod gate_hir;
pub mod import_hir;
pub mod moduleinterface_hir;
pub mod network_hir;
pub mod package_hir;
pub mod param_hir;
pub mod simple_module_hir;
pub mod submodules_hir;
pub mod type_inference;

pub mod section_order;
pub mod section_parser;
mod types_hir;

use section_parser::parse_module_sections;

use super::tree_utils::*;
use channel_hir::*;
use compound_module_hir::*;
pub use gate_hir::*;
use import_hir::*;
use moduleinterface_hir::*;
use network_hir::*;
use package_hir::*;
pub use param_hir::*;
use simple_module_hir::*;
use submodules_hir::*;
use tree_sitter::{Parser, Tree};

use ropey::Rope;
use tower_lsp::lsp_types::Url;

use std::hash::Hash;

use crate::db::{Db, DefKind};
use salsa::Accumulator;

use super::shared::*;

#[salsa::tracked(debug)]
pub struct NedFileHir<'db> {
    /// Uri
    #[returns(ref)]
    pub uri: Url,

    #[returns(ref)]
    pub package: Option<PackageHir<'db>>,

    #[returns(ref)]
    pub imports: Vec<ImportHir<'db>>,

    #[returns(ref)]
    pub items: Vec<ItemHir<'db>>,
}

#[allow(dead_code)]
pub trait Extract<T> {
    fn extract(&self) -> Option<&T>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)] // salsa::SalsaValue needed in order to
// avoid lifetime conflicts
pub enum ItemHir<'db> {
    Channel(ChannelHir<'db>),
    SimpleModule(SimpleModuleHir<'db>),
    CompoundModule(CompoundModuleHir<'db>),
    ModuleInterface(ModuleInterfaceHir<'db>),
    Network(NetworkHir<'db>),
    // Channel(ChannelHir),
    // ModuleInterface(ModuleInterfaceHir),
}

// TODO: should only contain span and name → the other attributes should be traits
impl<'db> ItemHir<'db> {
    pub fn name(&self, db: &'db dyn crate::db::Db) -> Word<'db> {
        match self {
            ItemHir::Channel(channel_hir) => *channel_hir.name(db),
            ItemHir::SimpleModule(simple_hir) => *simple_hir.name(db),
            ItemHir::CompoundModule(compound_hir) => *compound_hir.name(db),
            ItemHir::ModuleInterface(moduleinterface_hir) => *moduleinterface_hir.name(db),
            ItemHir::Network(network_hir) => *network_hir.name(db),
            // ItemHir::Channel(channel_hir) => channel_hir.name(db),
            // ItemHir::Network(network_hir) => network_hir.name(db),
            // ItemHir::ModuleInterface(iface_hir) => iface_hir.name(db),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ItemHir::Channel(_) => "channel",
            ItemHir::SimpleModule(_) => "simple",
            ItemHir::CompoundModule(_) => "module",
            ItemHir::ModuleInterface(_) => "moduleinterface",
            ItemHir::Network(_) => "network",
        }
    }

    #[allow(dead_code)]
    pub fn span(&self, db: &'db dyn crate::db::Db) -> Span {
        match self {
            ItemHir::Channel(channel_hir) => *channel_hir.span(db),
            ItemHir::SimpleModule(simple_hir) => *simple_hir.span(db),
            ItemHir::CompoundModule(compound_hir) => *compound_hir.span(db),
            ItemHir::ModuleInterface(moduleinterface_hir) => *moduleinterface_hir.span(db),
            ItemHir::Network(network_hir) => *network_hir.span(db),
        }
    }

    pub fn parameters(&self, db: &'db dyn crate::db::Db) -> &[ParamBlockItem<'db>] {
        match self {
            ItemHir::Channel(channel_hir) => channel_hir.parameters(db),
            ItemHir::SimpleModule(simple_hir) => simple_hir.parameters(db),
            ItemHir::CompoundModule(compound_hir) => compound_hir.parameters(db),
            ItemHir::ModuleInterface(moduleinterface_hir) => moduleinterface_hir.parameters(db),
            ItemHir::Network(network_hir) => network_hir.parameters(db),
        }
    }

    pub fn gates(&self, db: &'db dyn crate::db::Db) -> &[GateHir<'db>] {
        match self {
            ItemHir::Channel(_) => &[],
            ItemHir::SimpleModule(simple_hir) => simple_hir.gates(db),
            ItemHir::CompoundModule(compound_hir) => compound_hir.gates(db),
            ItemHir::ModuleInterface(moduleinterface_hir) => moduleinterface_hir.gates(db),
            ItemHir::Network(network_hir) => network_hir.gates(db),
        }
    }

    pub fn item_to_def(&self) -> DefKind {
        match self {
            ItemHir::Channel(_) => DefKind::Channel,
            ItemHir::SimpleModule(_) => DefKind::SimpleModule,
            ItemHir::CompoundModule(_) => DefKind::CompoundModule,
            ItemHir::ModuleInterface(_) => DefKind::ModuleInterface,
            ItemHir::Network(_) => DefKind::Network,
        }
    }

    pub fn extends(&self, db: &'db dyn crate::db::Db) -> Vec<&ExtendsHir<'db>> {
        match self {
            ItemHir::Channel(channel_hir) => {
                if let Some(extends) = channel_hir.extends(db) {
                    vec![extends]
                } else {
                    vec![]
                }
            }
            ItemHir::SimpleModule(simple_hir) => {
                if let Some(extends) = simple_hir.extends(db) {
                    vec![extends]
                } else {
                    vec![]
                }
            }
            ItemHir::CompoundModule(module_hir) => {
                if let Some(extends) = module_hir.extends(db) {
                    vec![extends]
                } else {
                    vec![]
                }
            }
            ItemHir::ModuleInterface(moduleinterface_hir) => {
                // Module interfaces can have multiple extends
                moduleinterface_hir.extends(db).iter().collect()
            }

            ItemHir::Network(network_hir) => {
                // Module interfaces can have multiple extends
                network_hir.extends(db).iter().collect()
            }
        }
    }

    #[allow(dead_code)]
    pub fn like(&self, db: &'db dyn crate::db::Db) -> Vec<LikeHir<'db>> {
        match self {
            ItemHir::SimpleModule(simple_hir) => simple_hir.like(db).clone(),
            ItemHir::Channel(channel_hir) => channel_hir.like(db).clone(),
            ItemHir::CompoundModule(compound_hir) => compound_hir.like(db).clone(),
            ItemHir::ModuleInterface(_) => Vec::new(),
            ItemHir::Network(_) => Vec::new(),
        }
    }

    pub fn types(&self, db: &'db dyn crate::db::Db) -> Vec<&ItemHir<'db>> {
        match self {
            ItemHir::CompoundModule(module_hir) => module_hir.types(db).iter().collect(),
            ItemHir::Network(network_hir) => network_hir.types(db).iter().collect(),
            _ => vec![],
        }
    }

    pub fn submodules(
        &self,
        db: &'db dyn crate::db::Db,
    ) -> Vec<&submodules_hir::SubmoduleHir<'db>> {
        match self {
            ItemHir::CompoundModule(module_hir) => module_hir.submodules(db).iter().collect(),
            ItemHir::Network(network_hir) => network_hir.submodules(db).iter().collect(),
            _ => vec![],
        }
    }
}

// INFO: Small test if this extract trait would sometimes simplify code
// Can be used like this: let concrete_item: Option<&crate::hir::compound_module_hir::CompoundModuleHir> = item.extract();
impl<'db> Extract<SimpleModuleHir<'db>> for ItemHir<'db> {
    fn extract(&self) -> Option<&SimpleModuleHir<'db>> {
        match self {
            ItemHir::SimpleModule(simple_module) => Some(simple_module),
            _ => None,
        }
    }
}

impl<'db> Extract<CompoundModuleHir<'db>> for ItemHir<'db> {
    fn extract(&self) -> Option<&CompoundModuleHir<'db>> {
        match self {
            ItemHir::CompoundModule(simple_module) => Some(simple_module),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue, Hash)]
pub struct ExtendsHir<'db> {
    pub name: Word<'db>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue, Hash)]
pub struct LikeHir<'db> {
    pub name: Word<'db>,
    pub span: Span,
}

#[allow(dead_code)]
fn print_node(node: tree_sitter::Node, indent: usize) {
    let indent_str = "  ".repeat(indent);

    // Byte ranges
    let start = node.start_byte();
    let end = node.end_byte();

    println!("{}{} [{}..{}]", indent_str, node.kind(), start, end);

    // Recurse into children
    for child in node.children(&mut node.walk()) {
        print_node(child, indent + 1);
    }
}

#[derive(Clone, Debug)]
pub struct NedTree(pub Tree);
impl PartialEq for NedTree {
    fn eq(&self, _other: &Self) -> bool {
        false
    }
}

impl Eq for NedTree {}

#[salsa::tracked]
// TODO: do not parse here because we are not using tree-sitter incremental parsing which would
// help for error recovery and performance. Instead, parse once when the file content changes
pub fn parse(db: &dyn Db, file: SourceFile) -> Option<NedTree> {
    let mut parser = Parser::new(); // TODO: reuse parser?
    parser.set_language(&tree_sitter_ned::language()).unwrap();
    let rope = file.rope(db);
    //
    let tree = parser.parse_with_options(
        &mut |byte_offset, _position| {
            if byte_offset >= rope.len_bytes() {
                return ""; // Return empty slice at EOF
            }
            let (chunk, chunk_byte_idx, _, _) = rope.chunk_at_byte(byte_offset);
            // We need the slice of the chunk starting from our offset
            &chunk[byte_offset - chunk_byte_idx..]
        },
        None,
        None, // could be used to set a progress callback
    )?;
    Some(NedTree(tree))
}

#[salsa::tracked]
pub fn lower_to_hir(db: &dyn crate::db::Db, ned: SourceFile) -> NedFileHir<'_> {
    // eprintln!("Lowering NED file to HIR: {:?}", ned.uri(db));
    // let tree = // parse(db, ned).unwrap().0;
    let tree = db
        .get_ast_for_file(&ned)
        .unwrap_or(parse(db, ned).clone().unwrap().0); // TODO: handle
    // parse

    let rope = ned.rope(db);
    let mut package: Option<PackageHir> = None;
    let mut imports = Vec::new();
    let mut definitions: Vec<ItemHir> = Vec::new();

    let root = tree.root_node();
    let mut cursor = root.walk();

    for node in root.children(&mut cursor) {
        match node.kind() {
            "package" => {
                // check for multiple package declarations
                if package.is_some() {
                    Diagnostic {
                        start: node.start_byte(),
                        end: node.end_byte(),
                        message: "Duplicate package declaration found. A package can only be declared once per file".to_string(),
                        version: *ned.version(db),
                        severity: crate::hir::Severity::Error,
url: Some(ned.uri(db).clone())
                    }
                    .accumulate(db);
                    continue;
                }

                let package_content = child_field_name(db, "name", node, rope);
                if let Some(package_content) = package_content {
                    package = Some(PackageHir {
                        path: package_content.word,
                        span: package_content.span,
                    });
                }
            }
            "import" => {
                if let Some(path) = child_field_name(db, "name", node, rope) {
                    imports.push(ImportHir {
                        value: path.word,
                        span: path.span,
                    });
                }
            }

            _ => {
                if let Some(definition_hir) =
                    parse_definition_node(db, node, rope, Some(ned.uri(db)), *ned.version(db))
                {
                    definitions.push(definition_hir);
                }
            }
        }
    }

    NedFileHir::new(db, ned.uri(db).clone(), package, imports, definitions)
}

pub fn parse_definition_node<'db>(
    db: &'db dyn crate::db::Db,
    node: tree_sitter::Node,
    rope: &Rope,
    uri: Option<&Url>,
    version: i32,
) -> Option<ItemHir<'db>> {
    match node.kind() {
        "simple" => {
            let simple_module_hir = build_simple_module_hir(db, node, rope, version)?;
            return Some(ItemHir::SimpleModule(simple_module_hir));
        }

        "channel" | "channel_interface" => {
            let channel_hir = build_channel_hir(db, node, rope, version)?;
            return Some(ItemHir::Channel(channel_hir));
        }

        "module" => {
            let module_hir = build_compound_module_hir(db, node, rope, version)?;
            return Some(ItemHir::CompoundModule(module_hir));
        }

        "network" => {
            let network_hir = build_network_hir(db, node, rope, version)?;
            return Some(ItemHir::Network(network_hir));
        }

        "moduleinterface" => {
            let module_interface_hir = build_module_interface_hir(db, node, rope, version)?;
            return Some(ItemHir::ModuleInterface(module_interface_hir));
        }

        "ERROR" => {
            // use salsa to accumulate diagnostics
            Diagnostic {
                severity: crate::hir::Severity::Error,

                start: node.start_byte(),
                end: node.end_byte(),
                message: "unexpected character ~ Root Level".to_string(),
                version,
                url: uri.cloned(),
            }
            .accumulate(db);
        }
        _ => {}
    }

    None
}

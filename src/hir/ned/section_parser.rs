use ropey::Rope;
use salsa::Accumulator;
use std::collections::HashMap;

use crate::{
    completion::ned::ContextType,
    db::Db,
    hir::{
        Diagnostic, Span, ned::connections_hir::build_connections_block,
        ned::gate_hir::build_gates_block, ned::param_hir::build_parameters_block,
        ned::section_order::SectionKind, ned::submodules_hir::build_submodules_block,
        ned::types_hir::build_types_block,
    },
};

pub struct ParsedSections<'db> {
    pub types: Vec<crate::hir::ItemHir<'db>>,
    pub parameters: Vec<super::ParamBlockItem<'db>>,
    pub gates: Vec<super::GateHir<'db>>,
    pub submodules: Vec<super::SubmoduleHir<'db>>,
    pub connections: Vec<super::connections_hir::ConnectionHir<'db>>,
}

/// A universal parser.
pub fn parse_module_sections<'db>(
    db: &'db dyn Db,
    node: tree_sitter::Node,
    rope: &Rope,
    version: i32,
    context: ContextType,
) -> ParsedSections<'db> {
    let mut sections = ParsedSections {
        types: Vec::new(),
        parameters: Vec::new(),
        gates: Vec::new(),
        submodules: Vec::new(),
        connections: Vec::new(),
    };

    let mut max_rank_seen: u8 = 0;
    let mut last_valid_section: Option<SectionKind> = None;
    let mut seen_sections: HashMap<SectionKind, Span> = HashMap::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind_str = child.kind();
        let current_span = Span::new(child.start_byte(), child.end_byte());

        if kind_str == "comment" || kind_str.trim().is_empty() {
            continue;
        }

        if kind_str == "ERROR" {
            Diagnostic {
                start: child.start_byte(),
                end: child.end_byte(),
                message: "Unexpected character inside module body.".to_string(),
                version,
                severity: crate::hir::Severity::Error,
                url: None,
            }
            .accumulate(db);
            continue;
        }

        let section_kind = kind_str
            .parse::<SectionKind>()
            .unwrap_or(SectionKind::Unknown);
        if section_kind == SectionKind::Unknown {
            continue;
        }

        if !section_kind.is_allowed_in(&context) {
            Diagnostic {
                severity: crate::hir::Severity::Error,
                start: current_span.start,
                end: current_span.end,
                message: format!("The '{}' section is not allowed here.", section_kind),
                version,
                url: None,
            }
            .accumulate(db);
            continue;
        }

        // check duplicates
        if let Some(previous_span) = seen_sections.get(&section_kind) {
            Diagnostic {
                severity: crate::hir::Severity::Error,
                start: current_span.start,
                end: current_span.end,
                message: format!("Duplicate '{}' block.", section_kind),
                version,
                url: None,
            }
            .accumulate(db);

            Diagnostic {
                severity: crate::hir::Severity::Error,
                start: previous_span.start,
                end: previous_span.end,
                message: "Original block defined here.".to_string(),
                version,
                url: None,
            }
            .accumulate(db);
            continue;
        }
        seen_sections.insert(section_kind, current_span);

        // check the ordering
        if let Some(rank) = section_kind.order_rank() {
            if rank < max_rank_seen {
                let previous_name = match last_valid_section {
                    Some(s) => s.to_string(),
                    None => "previous".to_string(),
                };
                Diagnostic {
                    severity: crate::hir::Severity::Error,
                    start: child.start_byte(),
                    end: child.end_byte(),
                    message: format!(
                        "'{}' block is out of order. It must come before the '{}' block.",
                        section_kind, previous_name
                    ),
                    version,
                    url: None,
                }
                .accumulate(db);
            } else {
                max_rank_seen = rank;
                last_valid_section = Some(section_kind);
            }
        }

        // build the individual section
        match section_kind {
            SectionKind::Types => {
                if let Some(t) = build_types_block(db, &child, rope, None, version) {
                    sections.types = t;
                }
            }
            SectionKind::Parameters => {
                if let Some(p) = build_parameters_block(db, &child, rope, version) {
                    sections.parameters = p;
                }
            }
            SectionKind::Gates => {
                if let Some(g) = build_gates_block(db, &child, rope, version) {
                    sections.gates = g;
                }
            }
            SectionKind::Submodules => {
                if let Some(s) = build_submodules_block(db, &child, rope, version) {
                    sections.submodules = s;
                }
            }
            SectionKind::Connections => {
                if let Some(c) = build_connections_block(db, &child, rope) {
                    sections.connections = c;
                }
            }
            SectionKind::Unknown => unreachable!(),
        }
    }

    sections
}

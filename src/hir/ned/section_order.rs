use std::{fmt, str::FromStr};

use crate::completion::ned::ContextType;

// TODO: check if I can reuse this for Channel, Interface, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, std::hash::Hash)]
pub enum SectionKind {
    Parameters,
    Gates,
    Types,
    Submodules,
    Connections,
    Unknown,
}

impl SectionKind {
    pub const ALL: [(SectionKind, &'static str); 5] = [
        (SectionKind::Parameters, "parameters:\n\t$0"),
        (SectionKind::Gates, "gates:\n\t$0"),
        (SectionKind::Types, "types:\n\t$0"),
        (SectionKind::Submodules, "submodules:\n\t$0"),
        (SectionKind::Connections, "connections:\n\t$0"),
    ];

    /// Checks if the section is allowed in the given context.
    pub fn is_allowed_in(&self, context: &ContextType) -> bool {
        match context {
            ContextType::SimpleModule | ContextType::ModuleInterface => {
                matches!(self, SectionKind::Parameters | SectionKind::Gates)
            }
            ContextType::Channel => {
                matches!(self, SectionKind::Parameters)
            }
            ContextType::CompoundModule | ContextType::Network => true,
            _ => false, // Default to false for completely unrelated contexts
        }
    }

    /// Defines the strict order. Lower numbers must appear first
    /// Returns None if the section does not have a defined order or the order is iligeal.
    pub fn order_rank(&self) -> Option<u8> {
        match self {
            SectionKind::Parameters => Some(0),
            SectionKind::Gates => Some(10),
            SectionKind::Types => Some(20),
            SectionKind::Submodules => Some(30),
            SectionKind::Connections => Some(40),
            _ => None,
        }
    }
}

impl FromStr for SectionKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "parameters" => Ok(SectionKind::Parameters),
            "gates" => Ok(SectionKind::Gates),
            "types" => Ok(SectionKind::Types),
            "submodules" => Ok(SectionKind::Submodules),
            "connections" => Ok(SectionKind::Connections),
            _ => Ok(SectionKind::Unknown),
        }
    }
}

impl fmt::Display for SectionKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match self {
            SectionKind::Parameters => "parameters",
            SectionKind::Gates => "gates",
            SectionKind::Types => "types",
            SectionKind::Submodules => "submodules",
            SectionKind::Connections => "connections",
            SectionKind::Unknown => "unknown",
        };
        write!(f, "{}", name)
    }
}

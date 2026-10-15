use std::{collections::HashMap, fmt};

use crate::{
    db::Db,
    hir::{
        self, Span, Word,
        ned::{import_hir::ImportPattern, package_hir::ResolvedPackage},
        shared::FilePath,
    },
    utils,
};

use crate::hir::ned::lower_to_hir;

use tower_lsp::lsp_types::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
#[allow(dead_code)]
pub enum DefKind {
    Package,
    Import,
    Property,
    Channel,
    SimpleModule,
    CompoundModule,
    Network,
    ModuleInterface,
    ChannelInterface,
    // Add others if necessary
}

impl fmt::Display for DefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind_str = match self {
            DefKind::Package => "package",
            DefKind::Import => "import",
            DefKind::Property => "property",
            DefKind::Channel => "channel",
            DefKind::SimpleModule => "simple module",
            DefKind::CompoundModule => "compound module",
            DefKind::Network => "network",
            DefKind::ModuleInterface => "module interface",
            DefKind::ChannelInterface => "channel interface",
        };
        write!(f, "{}", kind_str)
    }
}

#[salsa::tracked(debug)]
pub struct DefIndex<'db> {
    pub fqn: Word<'db>,
    pub simple_name: Word<'db>,
    pub uri: Url,
    pub kind: DefKind,
}

impl<'db> DefIndex<'db> {
    pub fn span(&self, db: &'db dyn Db) -> Span {
        // We only occasionally need the span for
        // goto_def, so we do not store it directly
        let ned_file = db.get_source_file_input(self.uri(db)).unwrap();
        let hir = lower_to_hir(db, ned_file);
        for item in hir.items(db).iter() {
            if item.name(db).text(db) == self.simple_name(db).text(db) {
                // Duplicated Names in same file
                // are not possible → TODO: add this to semantic diagnostics
                return item.span(db);
            }
        }
        Span::new(0, 0) // Fallback, should not happen
    }
}

// TODO: replace this with visible_defs
pub fn find_def<'db>(
    db: &'db dyn crate::db::Db,
    name: &str,
    imports: &[DefIndex<'db>],
) -> Option<DefIndex<'db>> {
    if name.contains(".") {
        // fqn is provided
        let pattern = ImportPattern::new(db, name);
        let result_opt = resolve_import(db, pattern);
        if let Some(results) = result_opt
            && results.len() == 1
        {
            return Some(results[0]);
        }
    } else {
        for import in imports.iter() {
            if import.simple_name(db).text(db).as_str() == name {
                return Some(*import);
            }
        }
    }
    None
}

// // INFO: I could also recompute in every query, because of the caching but as far as I understand
// // this would greate more dependencies that need to be invalidated on every change
// //
// /// List of definitions to easily pass around
// #[salsa::interned]
// pub struct DefinitionList<'db> {
//     #[ref]
//     pub defs: Vec<crate::db::DefIndex<'db>>,
// }
//

// INFO: it would be way cooler to turn the index into a word but then i would have to also turn the
// user input into a word making salsa interning do a lot of work for every search and saving those
// strings in memory

/// Indexes for optimized pattern matching
#[derive(Debug, Clone, salsa::SalsaValue, PartialEq)]
pub struct PackageIndexes<'db> {
    /// Maps simple names "Car" → to all declarations with that name
    pub suffix_index: HashMap<String, Vec<DefIndex<'db>>>,
    /// Maps any segment in FQN to all declarations containing that segment
    pub segment_index: HashMap<String, Vec<DefIndex<'db>>>,
    /// All declarations for patterns like "**"
    pub all_declarations: Vec<DefIndex<'db>>,
}

// I think I have massively overengineered this part and some things are not really a huge
// performance gain in comparison to the work salsa is doing in the background to check if the
// workspace_package_tree needs to be recomputed. But this is so far the best I could come up with.
// Same applies to this HashMap → maybe use a Word in the future
/// Tree structure representing package hierarchy
#[derive(Debug, Clone, salsa::SalsaValue, PartialEq)]
pub struct PackageTree<'db> {
    children: HashMap<String, PackageTree<'db>>,
    // If this node is a complete package → store its location
    decl: Option<DefIndex<'db>>,
}

impl<'db> PackageTree<'db> {
    pub fn new() -> Self {
        PackageTree {
            children: HashMap::new(),
            decl: None,
        }
    }

    pub fn merge(&mut self, other: PackageTree<'db>) {
        // If other has a declaration, take it
        if other.decl.is_some() {
            self.decl = other.decl;
        }

        // For children, only recurse if there's overlap
        for (key, other_child) in other.children {
            match self.children.entry(key) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    // Merge into existing child
                    entry.get_mut().merge(other_child);
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    // No overlap, just move the subtree
                    entry.insert(other_child);
                }
            }
        }
    }

    /// Insert fqn into the tree
    pub fn insert(&mut self, db: &dyn Db, decl: DefIndex<'db>) {
        let fqn = decl.fqn(db); // String lives for the rest of the function
        let fqn_str = fqn.text(db);
        let segments: Vec<&str> = fqn_str.split('.').collect();
        self.insert_recursive(&segments, decl);
    }

    fn insert_recursive(&mut self, segments: &[&str], decl: DefIndex<'db>) {
        if segments.is_empty() {
            self.decl = Some(decl);
            return;
        }

        let segment = segments[0].to_string();
        let child = self
            .children
            .entry(segment)
            .or_insert_with(PackageTree::new);
        child.insert_recursive(&segments[1..], decl);
    }

    /// Build multiple indexes for fast pattern matching
    pub fn build_indexes(&self, db: &dyn Db) -> PackageIndexes<'db> {
        let mut suffix_index: HashMap<String, Vec<DefIndex<'db>>> = HashMap::new();
        let mut segment_index: HashMap<String, Vec<DefIndex<'db>>> = HashMap::new();
        let mut all_declarations = Vec::new();

        self.build_indexes_recursive(
            db,
            &mut suffix_index,
            &mut segment_index,
            &mut all_declarations,
        );

        PackageIndexes {
            suffix_index,
            segment_index,
            all_declarations,
        }
    }

    fn build_indexes_recursive(
        &self,
        db: &dyn Db,
        suffix_index: &mut HashMap<String, Vec<DefIndex<'db>>>,
        segment_index: &mut HashMap<String, Vec<DefIndex<'db>>>,
        all_declarations: &mut Vec<DefIndex<'db>>,
    ) {
        if let Some(ref decl) = self.decl {
            // Index by simple name → using the last segment
            suffix_index
                .entry(decl.simple_name(db).text(db).clone())
                .or_default()
                .push(*decl);

            // Index by all segments in the FQN
            for segment in decl.fqn(db).text(db).split('.') {
                segment_index
                    .entry(segment.to_string())
                    .or_default()
                    .push(*decl);
            }

            // Keep all declarations for patterns like "**"
            all_declarations.push(*decl);
        }

        for child in self.children.values() {
            child.build_indexes_recursive(db, suffix_index, segment_index, all_declarations);
        }
    }

    /// Search for FQNs matching a wildcard pattern like "inet.applications.*" or "inet.**.tcp"
    pub fn search(&self, pattern: &str) -> Vec<DefIndex<'db>> {
        let segments: Vec<&str> = pattern.split('.').collect();

        // TODO: remove me → artifact from the first implementation
        if segments.contains(&"**") {
            return self.search_double_wildcard(&segments);
        }

        // Regular search for patterns without **
        let mut results = Vec::new();
        self.search_recursive(&segments, &mut results);
        results
    }

    /// Fast search using pre-built indexes for ** patterns
    pub fn search_with_indexes(
        &self,
        db: &dyn Db,
        pattern: &str,
        indexes: &PackageIndexes<'db>,
    ) -> Vec<DefIndex<'db>> {
        let segments: Vec<&str> = pattern.split('.').collect();

        if !segments.contains(&"**") {
            // No **, use regular search
            let mut results = Vec::new();
            self.search_recursive(&segments, &mut results);
            return results;
        }

        let double_star_pos = segments.iter().position(|&s| s == "**").unwrap();
        let prefix = &segments[..double_star_pos];
        let suffix = &segments[double_star_pos + 1..];

        // TODO: I do not really like this
        let prefix_has_wildcards = prefix.iter().any(|s| s.contains('*'));
        if prefix_has_wildcards {
            // Cannot do prefix navigation → fallback to full pattern matching
            let candidates = &indexes.all_declarations;
            return self.filter_by_complex_pattern(db, candidates, &segments);
        }

        // Analyze the pattern to choose the best strategy
        let double_star_count = segments.iter().filter(|&&s| s == "**").count();

        // Single ** → can optimize with indexes
        if double_star_count == 1 {
            if suffix.is_empty() {
                // Pattern like "org.**" - use prefix matching
                return self.search_prefix_wildcard(prefix);
            }

            // Pattern like "org.**.Car" - use suffix index
            if suffix.len() == 1 {
                let target_pattern = suffix[0];

                if !target_pattern.contains('*') {
                    // Exact match like "**.Car" - use suffix index
                    let default_empty = vec![];
                    let candidates = indexes
                        .suffix_index
                        .get(target_pattern)
                        .unwrap_or(&default_empty);

                    return self.filter_by_prefix(db, candidates, prefix);
                } else {
                    // Pattern like "**.Ca*" - need to check all declarations
                    // and filter by glob
                    let candidates = &indexes.all_declarations;

                    let filtered: Vec<DefIndex<'db>> = candidates
                        .iter()
                        .copied()
                        .filter(|decl| {
                            // Check if simple name matches the pattern
                            utils::match_pattern(target_pattern, decl.simple_name(db).text(db))
                        })
                        .collect();

                    return self.filter_by_prefix(db, &filtered, prefix);
                }
            }

            // Pattern like "org.**.nsm.Car" - use segment index for middle part
            if suffix.len() > 1 {
                // Get candidates from the first suffix segment
                let first_suffix_segment = suffix[0];
                let default_empty = vec![];
                let candidates = if first_suffix_segment.contains('*') {
                    // If it contains *, we need all declarations
                    &indexes.all_declarations
                } else {
                    indexes
                        .segment_index
                        .get(first_suffix_segment)
                        .unwrap_or(&default_empty)
                };

                return self.filter_by_prefix_and_suffix(db, candidates, prefix, suffix);
            }
        } else {
            // Multiple ** like "org.**.nsm.**"
            // I tried finding the fix segments to limit candidates
            let fixed_segments: Vec<&str> = segments
                .iter()
                .filter(|&&s| s != "**" && !s.contains('*'))
                .copied()
                .collect();

            if fixed_segments.is_empty() {
                // Pattern is "**" or "**.**...." return all
                return indexes.all_declarations.clone();
            }

            // Use the most specific segment to get candidates
            // For "org.**.nsm.**", use *nsm* as filter
            let default_empty = vec![];
            let candidates = if let Some(&segment) = fixed_segments.last() {
                indexes.segment_index.get(segment).unwrap_or(&default_empty)
            } else {
                &indexes.all_declarations
            };

            // Filter candidates by matching the full pattern
            return self.filter_by_complex_pattern(db, candidates, &segments);
        }

        // Fallback to BFS approach
        self.search_double_wildcard(&segments)
    }

    fn filter_by_prefix(
        &self,
        db: &dyn Db,
        candidates: &[DefIndex<'db>],
        prefix: &[&str],
    ) -> Vec<DefIndex<'db>> {
        candidates
            .iter()
            .copied()
            .filter(|decl| {
                let fqn = decl.fqn(db);
                let fqn_segments: Vec<&str> = fqn.text(db).split('.').collect();

                if fqn_segments.len() < prefix.len() {
                    return false;
                }

                for (i, &p) in prefix.iter().enumerate() {
                    if fqn_segments[i] != p {
                        return false;
                    }
                }

                true
            })
            .collect()
    }

    fn filter_by_prefix_and_suffix(
        &self,
        db: &dyn Db,
        candidates: &[DefIndex<'db>],
        prefix: &[&str],
        suffix: &[&str],
    ) -> Vec<DefIndex<'db>> {
        candidates
            .iter()
            .copied()
            .filter(|decl| {
                let fqn = &decl.fqn(db);
                let fqn_segments: Vec<&str> = fqn.text(db).split('.').collect();

                // Check if FQN is long enough
                if fqn_segments.len() < prefix.len() + suffix.len() {
                    return false;
                }

                // Check prefix matches
                for (i, &p) in prefix.iter().enumerate() {
                    if fqn_segments[i] != p {
                        return false;
                    }
                }

                // Check suffix matches → looking from the end
                let suffix_start = fqn_segments.len() - suffix.len();
                for (i, &s) in suffix.iter().enumerate() {
                    if s.contains('*') {
                        // Handle wildcards in suffix
                        if !utils::match_pattern(s, fqn_segments[suffix_start + i]) {
                            return false;
                        }
                    } else if fqn_segments[suffix_start + i] != s {
                        return false;
                    }
                }

                true
            })
            .collect()
    }

    fn filter_by_complex_pattern(
        &self,
        db: &dyn Db,
        candidates: &[DefIndex<'db>],
        pattern_segments: &[&str],
    ) -> Vec<DefIndex<'db>> {
        candidates
            .iter()
            .copied()
            .filter(|decl| self.matches_complex_pattern(decl.fqn(db).text(db), pattern_segments))
            .collect()
    }

    /// Match a complex pattern like "org.**.nsm.**" against an FQN
    fn matches_complex_pattern(&self, fqn: &str, pattern: &[&str]) -> bool {
        let fqn_segments: Vec<&str> = fqn.split('.').collect();
        PackageTree::match_pattern_recursive(&fqn_segments, 0, pattern, 0)
    }

    fn match_pattern_recursive(
        fqn: &[&str],
        fqn_idx: usize,
        pattern: &[&str],
        pat_idx: usize,
    ) -> bool {
        // Both exhausted → match
        if pat_idx >= pattern.len() && fqn_idx >= fqn.len() {
            return true;
        }

        // No Pattern left no match
        if pat_idx >= pattern.len() {
            return false;
        }

        // FQN exhausted but pattern not → only match if remaining pattern is ** or *
        if fqn_idx >= fqn.len() {
            return pattern[pat_idx..].iter().all(|&s| s == "**");
        }

        let current_pattern = pattern[pat_idx];

        match current_pattern {
            "**" => {
                // Try matching the rest of the pattern at every position
                for i in fqn_idx..=fqn.len() {
                    if PackageTree::match_pattern_recursive(fqn, i, pattern, pat_idx + 1) {
                        return true;
                    }
                }
                false
            }
            "*" => {
                // Match exactly one segment
                PackageTree::match_pattern_recursive(fqn, fqn_idx + 1, pattern, pat_idx + 1)
            }
            _ => {
                // Exact match or regex match
                let matches = if current_pattern.contains('*') {
                    utils::match_pattern(current_pattern, fqn[fqn_idx])
                } else {
                    fqn[fqn_idx] == current_pattern
                };

                if matches {
                    PackageTree::match_pattern_recursive(fqn, fqn_idx + 1, pattern, pat_idx + 1)
                } else {
                    false
                }
            }
        }
    }

    // If not indexes are available → fallback
    fn search_double_wildcard(&self, segments: &[&str]) -> Vec<DefIndex<'db>> {
        let mut results = Vec::new();

        // Find the position of **
        let double_star_pos = segments.iter().position(|&s| s == "**").unwrap();

        // Split pattern: [prefix, **, suffix]
        let prefix = &segments[..double_star_pos];
        let suffix = &segments[double_star_pos + 1..];

        // Navigate to the prefix
        let mut current = self;
        for &segment in prefix {
            if let Some(child) = current.children.get(segment) {
                current = child;
            } else {
                return vec![]; // No prefix found 
            }
        }

        // Now do BFS from current node, matching suffix
        if suffix.is_empty() {
            // Pattern like "org.**" → collect everything
            current.collect_all(&mut results);
        } else {
            // INFO: This was the greatest performance gain
            // Pattern like "org.**.Car" - match suffix at all levels
            self.bfs_match_suffix(current, suffix, &mut results);
        }

        results
    }

    /// Breadth-first search matching suffix at all descendant levels
    fn bfs_match_suffix(
        &self,
        root: &PackageTree<'db>,
        suffix: &[&str],
        results: &mut Vec<DefIndex<'db>>,
    ) {
        use std::collections::VecDeque;

        let mut queue = VecDeque::new();
        queue.push_back(root);

        while let Some(node) = queue.pop_front() {
            // Try to match suffix from this node
            let mut current = node;
            let mut matched = true;

            for &segment in suffix {
                if let Some(child) = current.children.get(segment) {
                    current = child;
                } else {
                    matched = false;
                    break;
                }
            }

            if matched && let Some(ref decl) = current.decl {
                results.push(*decl);
            }

            // Add all children to queue for next level
            for child in node.children.values() {
                queue.push_back(child);
            }
        }
    }

    fn search_prefix_wildcard(&self, prefix: &[&str]) -> Vec<DefIndex<'db>> {
        let mut current = self;

        for &segment in prefix {
            if let Some(child) = current.children.get(segment) {
                current = child;
            } else {
                return vec![];
            }
        }

        let mut results = Vec::new();
        current.collect_all(&mut results);
        results
    }

    fn search_recursive(&self, pattern_segments: &[&str], results: &mut Vec<DefIndex<'db>>) {
        if pattern_segments.is_empty() {
            // Pattern exhausted, collect everything under this node
            self.collect_all(results);
            return;
        }

        let current_segment = pattern_segments[0];
        let remaining = &pattern_segments[1..];

        match current_segment {
            "*" => {
                // Single wildcard: match all immediate children
                if remaining.is_empty() {
                    // Last segment is *, collect immediate definitions
                    for child in self.children.values() {
                        if let Some(ref decl) = child.decl {
                            results.push(*decl);
                        }
                    }
                } else {
                    // Continue pattern matching in all children
                    for child in self.children.values() {
                        child.search_recursive(remaining, results);
                    }
                }
            }
            "**" => {
                // This case is now handled by search_double_wildcard
                // But I use this as a fallback
                if remaining.is_empty() {
                    self.collect_all(results);
                } else {
                    self.search_recursive(remaining, results);
                    for child in self.children.values() {
                        child.search_recursive(pattern_segments, results);
                    }
                }
            }
            exact => {
                // check if string contains wildcard characters
                if exact.contains('*') {
                    // check hashmap keys for matching patterns
                    for (key, child) in &self.children {
                        if utils::match_pattern(exact, key) {
                            if remaining.is_empty() {
                                if let Some(ref decl) = child.decl {
                                    results.push(*decl);
                                }
                            } else {
                                child.search_recursive(remaining, results);
                            }
                        }
                    }
                    return;
                }

                // Exact match: follow specific child
                if let Some(child) = self.children.get(exact) {
                    if remaining.is_empty() {
                        if let Some(ref decl) = child.decl {
                            results.push(*decl);
                        }
                    } else {
                        child.search_recursive(remaining, results);
                    }
                }
            }
        }
    }

    /// Collect all FQNs under this node
    fn collect_all(&self, results: &mut Vec<DefIndex<'db>>) {
        if let Some(ref decl) = self.decl {
            results.push(*decl);
        }
        for child in self.children.values() {
            child.collect_all(results);
        }
    }

    pub fn suggest_completions(
        &self,
        db: &dyn Db,
        prefix_str: &str,
        indexes: &PackageIndexes<'db>,
    ) -> Vec<DefIndex<'db>> {
        // Fallback with wildcards: utilize the existing search
        if prefix_str.contains('*') {
            return self.search_with_indexes(db, prefix_str, indexes);
        }

        let mut results = Vec::new();

        // no dots → unqualified search
        if !prefix_str.contains('.') {
            // Find anything where the simple name starts with the prefix
            // Because I want to allow typing just RSU and get suggestions for the full name like
            // "org.car2x.veins.nodes.RSU"
            for decl in &indexes.all_declarations {
                if decl.simple_name(db).text(db).starts_with(prefix_str) {
                    results.push(*decl);
                }
            }
            // Also match root packages that start with the prefix
            for (name, child) in &self.children {
                if name.starts_with(prefix_str) {
                    child.collect_all(&mut results);
                }
            }

            // Deduplicate based on fully qualified name
            results.sort_by_key(|a| a.fqn(db).text(db));
            results.dedup();
            return results;
        }

        // For qualified match → traverse the tree
        let segments: Vec<&str> = prefix_str.split('.').collect();
        let mut current = self;
        let last_idx = segments.len() - 1;

        // Traverse exact segments
        for &segment in &segments[..last_idx] {
            if let Some(child) = current.children.get(segment) {
                current = child;
            } else {
                return vec![]; // Prefix path not found
            }
        }

        // For the last partial segment, collect children that start with it
        let partial = segments[last_idx];
        for (name, child) in &current.children {
            if name.starts_with(partial) {
                child.collect_all(&mut results);
            }
        }

        results
    }
}

/// Get all definitions in a file with their FQNs
#[salsa::tracked]
pub fn file_def_index<'db>(db: &'db dyn Db, ned_file: hir::SourceFile) -> Vec<DefIndex<'db>> {
    let hir = lower_to_hir(db, ned_file);
    let mut defs = Vec::new();

    let package_prefix = hir
        .package(db)
        .map(|pkg| format!("{}.", pkg.path.text(db)))
        .unwrap_or_else(|| {
            if let Ok(file_path) = ned_file.uri(db).to_file_path() {
                let file_dir = file_path.parent().unwrap_or(&file_path);
                let interned_dir = FilePath::new(db, file_dir.to_path_buf());
                let pkg = crate::semantics::ned::determine_expected_package(db, interned_dir);
                let pkg_name = match pkg {
                    ResolvedPackage::FromPath(p) => p.name(db),
                    ResolvedPackage::FromName(p) => p.name(db),
                };
                if pkg_name.is_empty() {
                    "".to_string()
                } else {
                    format!("{}.", pkg_name)
                }
            } else {
                "".to_string()
            }
        });

    for item in hir.items(db).iter() {
        let name_str = item.name(db).text(db);
        let name = Word::new(db, name_str);
        let fqn_string = format!("{}{}", package_prefix, name_str);
        let fqn = hir::Word::new(db, fqn_string);
        defs.push(DefIndex::new(
            db,
            fqn,
            name,
            ned_file.uri(db).clone(),
            item.item_to_def(),
        ));
    }
    defs
}

#[salsa::tracked]
fn file_package_tree<'db>(db: &'db dyn Db, ned_file: hir::SourceFile) -> PackageTree<'db> {
    let mut tree = PackageTree::new();

    for decl in file_def_index(db, ned_file) {
        tree.insert(db, *decl);
    }

    tree
}

#[salsa::tracked]
pub fn workspace_package_tree<'db>(db: &'db dyn Db) -> std::sync::Arc<PackageTree<'db>> {
    let _ = db.register_for_workspace_updates();

    let mut tree = PackageTree::new();
    let ned_files = db.get_all_source_file_inputs();

    for ned_file in ned_files {
        tree.merge(file_package_tree(db, ned_file).clone());
    }

    std::sync::Arc::new(tree)
}

#[salsa::tracked]
pub fn workspace_indexes<'db>(db: &'db dyn Db) -> std::sync::Arc<PackageIndexes<'db>> {
    let tree = workspace_package_tree(db);
    std::sync::Arc::new(tree.build_indexes(db))
}

#[salsa::tracked]
pub fn resolve_import<'db>(
    db: &'db dyn Db,
    import: ImportPattern<'db>,
) -> Option<Vec<DefIndex<'db>>> {
    let pattern = import.pattern(db);

    // let built_in_types = [
    //     "ned.channels",
    //     "ned.DatarateChannel",
    //     "ned.IdealChannel",
    //     "ned.DelayChannel",
    // ];
    // let built_in_t_check: Option<Vec<&str>> = built_in_types
    //     .iter()
    //     .copied()
    //     .find(|&x| x == pattern)
    //     .map(|x| vec![x]);
    //
    // // TODO: maybe I can provide some hover information
    // if let Some(_built_in) = built_in_t_check {
    //     return Some(Vec::new());
    // }

    // Use optimized search with indexes for ** patterns
    let matches = if pattern.contains("**") {
        let tree = workspace_package_tree(db);
        let indexes = workspace_indexes(db);
        tree.search_with_indexes(db, pattern, indexes)
    } else {
        let tree = workspace_package_tree(db);
        tree.search(pattern)
    };

    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

#[salsa::tracked]
pub fn resolve_global_type<'db>(db: &'db dyn Db, type_name: Word<'db>) -> Option<DefIndex<'db>> {
    let indexes = workspace_indexes(db);
    indexes
        .suffix_index
        .get(type_name.text(db))
        .and_then(|defs| defs.first().copied())
}

#[cfg(test)]
mod tests {
    use tower_lsp::lsp_types::Url;

    use super::*;
    use crate::db;

    // DefIndex is a Salsa tracked struct and must be created inside a tracked function.
    #[salsa::tracked]
    fn package_tree_test_body(db: &dyn crate::db::Db) {
        let mut tree = PackageTree::new();
        let url = Url::parse("file:///test.ned").unwrap();

        let tcp_app_name = Word::new(db, "TcpApp".to_string());
        let tcp_app_fqn = Word::new(db, "inet.applications.TcpApp".to_string());
        let tcp_app = DefIndex::new(
            db,
            tcp_app_fqn,
            tcp_app_name,
            url.clone(),
            DefKind::SimpleModule,
        );
        tree.insert(db, tcp_app);

        let udp_app_name = Word::new(db, "UdpApp".to_string());
        let udp_app_fqn = Word::new(db, "inet.applications.UdpApp".to_string());
        let udp_app = DefIndex::new(
            db,
            udp_app_fqn,
            udp_app_name,
            url.clone(),
            DefKind::SimpleModule,
        );
        tree.insert(db, udp_app);

        let car_name = Word::new(db, "Car".to_string());
        let car_fqn = Word::new(db, "inet.nodes.Car".to_string());
        let car = DefIndex::new(db, car_fqn, car_name, url.clone(), DefKind::SimpleModule);
        tree.insert(db, car);

        let results = tree.search("inet.applications.TcpApp");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], tcp_app);

        let results = tree.search("inet.applications.*");
        assert_eq!(results.len(), 2);
        assert!(results.contains(&tcp_app));
        assert!(results.contains(&udp_app));

        let results = tree.search("inet.*.*");
        assert_eq!(results.len(), 3);

        let results = tree.search("inet.**");
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_package_tree() {
        let db = db::RootLspDatabase::default();
        package_tree_test_body(&db);
    }
}

use std::path::{Component, Path, PathBuf};

use crate::Config;

/// Why a file is (not) part of the indexed scope
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Membership {
    /// The file lives under one of the configured nedfolders → fully indexed.
    Indexed,
    /// The owning project is known, but the files folder is not listed in its .nedfolders
    FolderNotListed {
        project_root: PathBuf,
        folder: PathBuf,
        /// folder relative to project_root, as it would be written into .nedfolders
        rel: String,
    },
    /// The file sits in a .project that is neither the workspace root nor an imported project
    ProjectNotImported {
        project_root: PathBuf,
        /// Directory name of the project →  for user facing messages.
        name: String,
    },
    /// No .project marker was found anywhere above the file
    NoProject { folder: PathBuf },
}

/// Stable identifier attached to the LSP diagnostic, and the key the code action handler
/// matches on.
impl Membership {
    pub fn diagnostic_code(&self) -> Option<&'static str> {
        match self {
            Membership::Indexed => None,
            Membership::FolderNotListed { .. } => Some("unindexed-folder"),
            Membership::ProjectNotImported { .. } => Some("unimported-project"),
            Membership::NoProject { .. } => Some("no-project"),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Membership::Indexed => String::new(),
            Membership::FolderNotListed {
                project_root, rel, ..
            } => format!(
                "'{}' is not listed in {}/.nedfolders, so only this file is indexed. \
                 Imports and cross-file references will not resolve.",
                rel,
                project_root.display()
            ),
            Membership::ProjectNotImported { name, .. } => format!(
                "This file belongs to project '{name}', which is not part of the current workspace. \
                 Only this file is indexed until the project is added to Ned_ls.toml."
            ),
            Membership::NoProject { folder } => format!(
                "'{}' is not part of any OMNeT++ project (no .project file found) and is not \
                 covered by a nedfolder, so only this file is indexed.",
                folder.display()
            ),
        }
    }
}

pub fn nedfolder_root_for<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a PathBuf> {
    roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.as_os_str().len())
}

/// Strip `.` components and canonicalize when the path exists.
pub fn normalize(path: &Path) -> PathBuf {
    let cleaned: PathBuf = path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();

    std::fs::canonicalize(&cleaned).unwrap_or(cleaned)
}

/// Classify a file against the current configuration.
pub fn classify(file_path: &Path, config: &Config) -> Membership {
    let file_path = normalize(file_path);

    if nedfolder_root_for(&file_path, &config.nedfolders).is_some() {
        return Membership::Indexed;
    }

    let folder = file_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| file_path.clone());

    let Some(project_root) = crate::utils::find_file_upwards(&file_path, ".project") else {
        return Membership::NoProject { folder };
    };
    let project_root = normalize(&project_root);

    let workspace_root = config
        .project_root
        .to_file_path()
        .ok()
        .map(|p| normalize(&p));

    // println!("classify: file_path={file_path:?}, folder={folder:?}, project_root={project_root:?}, workspace_root={workspace_root:?}, imported_projects={:?}", config.imported_projects);

    let is_known = workspace_root.as_deref() == Some(project_root.as_path())
        || config.imported_projects.contains(&project_root);

    // println!("classify: {is_known}");
    if is_known {
        let rel = folder
            .strip_prefix(&project_root)
            .ok()
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| ".".to_string());

        Membership::FolderNotListed {
            project_root,
            folder,
            rel,
        }
    } else {
        let name = project_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| project_root.display().to_string());

        Membership::ProjectNotImported { project_root, name }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use tower_lsp::lsp_types::Url;

    fn config(root: &Path, nedfolders: Vec<PathBuf>, imported: Vec<PathBuf>) -> Config {
        Config {
            imported_projects: imported
                .iter()
                .map(|p| normalize(p))
                .collect::<HashSet<_>>(),
            project_root: Url::from_file_path(root).unwrap(),
            nedfolders: nedfolders.iter().map(|p| normalize(p)).collect(),
        }
    }

    /// <tmp>/proj/{.project, src/a.ned, simulations/b.ned}
    fn project(tmp: &Path) -> PathBuf {
        let proj = tmp.join("proj");
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::create_dir_all(proj.join("simulations")).unwrap();
        fs::write(proj.join(".project"), "").unwrap();
        fs::write(proj.join("src/a.ned"), "").unwrap();
        fs::write(proj.join("simulations/b.ned"), "").unwrap();
        proj
    }

    #[test]
    fn file_inside_a_nedfolder_is_indexed() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = project(tmp.path());
        let cfg = config(&proj, vec![proj.join("simulations")], vec![]);

        assert_eq!(
            classify(&proj.join("simulations/b.ned"), &cfg),
            Membership::Indexed
        );
    }

    #[test]
    fn folder_of_known_project_not_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = project(tmp.path());
        let cfg = config(&proj, vec![proj.join("simulations")], vec![]);

        match classify(&proj.join("src/a.ned"), &cfg) {
            Membership::FolderNotListed {
                project_root, rel, ..
            } => {
                assert_eq!(project_root, normalize(&proj));
                assert_eq!(rel, "src");
            }
            other => panic!("expected FolderNotListed, got {other:?}"),
        }
    }

    #[test]
    fn curdir_suffixed_nedfolder_root_still_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = project(tmp.path());
        let cfg = config(&proj, vec![proj.join(".")], vec![]);

        assert_eq!(
            classify(&proj.join("src/a.ned"), &cfg),
            Membership::Indexed,
            "the '<root>/.' fallback shape must cover the whole project"
        );
    }

    #[test]
    fn canonicalized_and_raw_paths_compare_equal() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = project(tmp.path());
        let noisy = proj.join("./src/./");
        let cfg = config(&proj, vec![noisy], vec![]);

        assert_eq!(classify(&proj.join("src/a.ned"), &cfg), Membership::Indexed);
    }

    #[test]
    fn file_in_foreign_project() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = project(tmp.path());

        let other = tmp.path().join("inet");
        fs::create_dir_all(other.join("src")).unwrap();
        fs::write(other.join(".project"), "").unwrap();
        fs::write(other.join("src/c.ned"), "").unwrap();

        let cfg = config(&proj, vec![proj.join(".")], vec![]);

        //
        match classify(&other.join("src/c.ned"), &cfg) {
            Membership::ProjectNotImported { name, project_root } => {
                assert_eq!(name, "inet");
                assert_eq!(project_root, normalize(&other));
            }
            other => panic!("expected ProjectNotImported, got {other:?}"),
        }
    }

    #[test]
    fn file_without_any_project_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = project(tmp.path());

        let loose = tmp.path().join("loose");
        fs::create_dir_all(&loose).unwrap();
        fs::write(loose.join("d.ned"), "").unwrap();

        let cfg = config(&proj, vec![proj.join(".")], vec![]);

        //println!("loose={:?}, cfg={:?}", loose, cfg);

        match classify(&loose.join("d.ned"), &cfg) {
            Membership::NoProject { folder } => assert_eq!(folder, normalize(&loose)),
            other => panic!("expected NoProject, got {other:?}"),
        }
    }

    #[test]
    fn imported_project_counts_as_known() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = project(tmp.path());

        let other = tmp.path().join("inet");
        fs::create_dir_all(other.join("src")).unwrap();
        fs::write(other.join(".project"), "").unwrap();
        fs::write(other.join("src/c.ned"), "").unwrap();

        // imported
        let cfg = config(
            &proj,
            vec![proj.join("."), other.join("simulations")],
            vec![other.clone()],
        );

        match classify(&other.join("src/c.ned"), &cfg) {
            Membership::FolderNotListed { rel, .. } => assert_eq!(rel, "src"),
            other => panic!("expected FolderNotListed, got {other:?}"),
        }
    }
}

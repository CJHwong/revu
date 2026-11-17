//! The rows of the Files changed list: a folder tree like GitHub's, or the flat path list.

use std::collections::{BTreeMap, HashSet};

use crate::model::ChangedFile;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRow {
    /// A folder. `label` can span several levels ("prompts/github") when each level holds
    /// only the next one, as GitHub shows it.
    Dir {
        path: String,
        label: String,
        depth: usize,
        folded: bool,
    },
    /// `index` points into the PR's file list.
    File { index: usize, depth: usize },
}

#[derive(Default)]
struct Folder {
    folders: BTreeMap<String, Folder>,
    files: Vec<usize>,
}

/// `shown` holds the indexes of the files to list, in order. A folded folder lists none of
/// its contents.
pub fn rows(
    files: &[ChangedFile],
    shown: &[usize],
    tree: bool,
    folded: &HashSet<String>,
) -> Vec<FileRow> {
    if !tree {
        let flat = shown.iter().map(|index| FileRow::File {
            index: *index,
            depth: 0,
        });
        return flat.collect();
    }
    let mut root = Folder::default();
    for index in shown {
        let mut folder = &mut root;
        let mut parts: Vec<&str> = files[*index].path.split('/').collect();
        parts.pop();
        for part in parts {
            folder = folder.folders.entry(part.to_string()).or_default();
        }
        folder.files.push(*index);
    }
    let mut rows = Vec::new();
    add_folder(&root, "", 0, folded, &mut rows);
    rows
}

/// Folders first, then files, as GitHub orders them.
fn add_folder(
    folder: &Folder,
    path: &str,
    depth: usize,
    folded: &HashSet<String>,
    rows: &mut Vec<FileRow>,
) {
    for (name, child) in &folder.folders {
        let (label, child) = compress(name, child);
        let full = if path.is_empty() {
            label.clone()
        } else {
            format!("{path}/{label}")
        };
        let is_folded = folded.contains(&full);
        rows.push(FileRow::Dir {
            path: full.clone(),
            label,
            depth,
            folded: is_folded,
        });
        if !is_folded {
            add_folder(child, &full, depth + 1, folded, rows);
        }
    }
    rows.extend(folder.files.iter().map(|index| FileRow::File {
        index: *index,
        depth,
    }));
}

/// Joins a chain of folders that each hold only one folder into one label.
fn compress<'a>(name: &str, mut folder: &'a Folder) -> (String, &'a Folder) {
    let mut label = name.to_string();
    while folder.files.is_empty() && folder.folders.len() == 1 {
        let Some((next, child)) = folder.folders.iter().next() else {
            break;
        };
        label = format!("{label}/{next}");
        folder = child;
    }
    (label, folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ViewedState;

    fn file(path: &str) -> ChangedFile {
        ChangedFile {
            path: path.to_string(),
            additions: 1,
            deletions: 0,
            change: "modified".to_string(),
            viewed: ViewedState::Unviewed,
        }
    }

    fn labels(rows: &[FileRow], files: &[ChangedFile]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                FileRow::Dir { label, depth, .. } => format!("{}{label}/", "  ".repeat(*depth)),
                FileRow::File { index, depth } => {
                    let name = files[*index].path.rsplit('/').next().unwrap_or_default();
                    format!("{}{name}", "  ".repeat(*depth))
                }
            })
            .collect()
    }

    #[test]
    fn the_tree_joins_single_folder_chains_and_lists_folders_first() {
        let files = vec![
            file(".github/workflows/ci.yml"),
            file("README.md"),
            file("prompts/github/_default.md"),
            file("prompts/github/review.md"),
            file("prompts/jira.md"),
        ];
        let shown: Vec<usize> = (0..files.len()).collect();
        let rows = rows(&files, &shown, true, &HashSet::new());
        assert_eq!(
            labels(&rows, &files),
            [
                ".github/workflows/",
                "  ci.yml",
                "prompts/",
                "  github/",
                "    _default.md",
                "    review.md",
                "  jira.md",
                "README.md",
            ]
        );
    }

    #[test]
    fn a_folded_folder_hides_its_files_and_flat_lists_every_path() {
        let files = vec![file("a/one.md"), file("a/two.md"), file("b.md")];
        let shown = [0, 1, 2];
        let folded = HashSet::from(["a".to_string()]);
        assert_eq!(
            labels(&rows(&files, &shown, true, &folded), &files),
            ["a/", "b.md"]
        );
        assert_eq!(rows(&files, &shown, false, &folded).len(), 3);
    }
}

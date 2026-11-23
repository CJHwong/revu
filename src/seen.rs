//! What each PR looked like when you last left it, so the inbox marks the PRs that got
//! new commits or comments since.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::PrSummary;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub head: String,
    pub comments: u64,
}

/// Marks by `owner/repo#number`.
pub type Marks = HashMap<String, Mark>;

#[derive(Debug, Default)]
pub struct Seen {
    marks: Marks,
    /// Marks set since the last save. Only these go to the file, so another revu's
    /// marks on disk survive.
    unsaved: Marks,
}

fn key(pr: &PrSummary) -> String {
    format!("{}#{}", pr.repo, pr.number)
}

impl Seen {
    pub fn new(marks: Marks) -> Self {
        Self {
            marks,
            unsaved: Marks::new(),
        }
    }

    pub fn record(&mut self, pr: &PrSummary) {
        let mark = Mark {
            head: pr.head_oid.clone(),
            comments: pr.comments,
        };
        if self.marks.get(&key(pr)) == Some(&mark) {
            return;
        }
        self.unsaved.insert(key(pr), mark.clone());
        self.marks.insert(key(pr), mark);
    }

    /// A PR you never opened has no mark, so it is not new: the first run marks nothing.
    /// A deleted comment lowers the count, which is not news.
    pub fn is_new(&self, pr: &PrSummary) -> bool {
        self.marks
            .get(&key(pr))
            .is_some_and(|mark| mark.head != pr.head_oid || pr.comments > mark.comments)
    }

    pub fn take_unsaved(&mut self) -> Marks {
        std::mem::take(&mut self.unsaved)
    }
}

pub fn path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("seen.yaml")
}

/// The saved marks, or none when the file does not exist yet.
pub fn load(path: &Path) -> Result<Marks> {
    if !path.exists() {
        return Ok(Marks::new());
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

/// Lays `changes` over the marks on disk and writes the result.
pub fn save(path: &Path, changes: Marks) -> Result<()> {
    let mut marks = load(path)?;
    marks.extend(changes);
    let text = format!(
        "# Written by revu: each PR's head commit and comment count when you last left it.\n{}",
        serde_yaml::to_string(&marks)?
    );
    fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReviewState;
    use chrono::Utc;

    fn pr(head: &str, comments: u64) -> PrSummary {
        PrSummary {
            repo: "acme/app".into(),
            number: 7,
            title: "Fix".into(),
            url: String::new(),
            author: "octo".into(),
            updated_at: Utc::now(),
            is_draft: false,
            review: ReviewState::Awaiting,
            checks: None,
            comments,
            additions: 0,
            deletions: 0,
            changed_files: 0,
            head_oid: head.into(),
            turn: None,
            conflicts: false,
            merge_state: String::new(),
            base: "main".into(),
            head: "feature".into(),
            cross_repo: false,
            default_branch: "main".to_string(),
        }
    }

    #[test]
    fn a_pr_is_new_after_a_push_or_a_comment_since_you_left_it() {
        let mut seen = Seen::default();
        assert!(!seen.is_new(&pr("a", 1)), "never opened");
        seen.record(&pr("a", 1));
        assert!(!seen.is_new(&pr("a", 1)));
        assert!(seen.is_new(&pr("b", 1)), "a push");
        assert!(seen.is_new(&pr("a", 2)), "a comment");
        assert!(!seen.is_new(&pr("a", 0)), "a deleted comment");
    }

    #[test]
    fn a_save_keeps_the_marks_another_revu_wrote() {
        let dir = std::env::temp_dir().join(format!("revu-seen-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = path(&dir.join("config.yaml"));
        assert_eq!(load(&file).unwrap(), Marks::new(), "no file yet");

        let mut other = Seen::default();
        other.record(&PrSummary {
            number: 8,
            ..pr("x", 3)
        });
        save(&file, other.take_unsaved()).unwrap();
        let mut ours = Seen::default();
        ours.record(&pr("a", 1));
        save(&file, ours.take_unsaved()).unwrap();
        assert!(ours.take_unsaved().is_empty(), "a save empties the queue");

        let reloaded = Seen::new(load(&file).unwrap());
        assert!(reloaded.is_new(&PrSummary {
            number: 8,
            ..pr("y", 3)
        }));
        assert!(reloaded.is_new(&pr("a", 2)));

        fs::write(&file, "not: [yaml").unwrap();
        assert!(
            load(&file).is_err(),
            "a corrupt file is an error, not an empty map"
        );
        fs::remove_file(&file).unwrap();
        fs::remove_dir(&dir).unwrap();
    }
}

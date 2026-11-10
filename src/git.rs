//! Git worktree/repository identity without spawning git.
//!
//! * worktree: the top-level directory of the checkout containing `cwd`.
//! * repo: the shared `.git` directory; identical for every worktree of one repository.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::config::expand;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GitLocation {
    pub worktree: Option<String>,
    pub repo: Option<String>,
}

thread_local! {
    static CACHE: RefCell<HashMap<String, GitLocation>> = RefCell::new(HashMap::new());
}

pub fn locate(cwd: &str) -> GitLocation {
    if cwd.is_empty() {
        return GitLocation::default();
    }
    if let Some(hit) = CACHE.with_borrow(|c| c.get(cwd).cloned()) {
        return hit;
    }
    let loc = find(&expand(cwd));
    CACHE.with_borrow_mut(|c| c.insert(cwd.to_string(), loc.clone()));
    loc
}

fn find(path: &Path) -> GitLocation {
    for directory in path.ancestors() {
        let dotgit = directory.join(".git");
        let repo = match fs::metadata(&dotgit) {
            Ok(m) if m.is_dir() => Some(real(&dotgit)),
            Ok(m) if m.is_file() => common_dir(&dotgit),
            Ok(_) => continue,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => continue,
            Err(_) => return GitLocation::default(),
        };
        return GitLocation { worktree: Some(directory.to_string_lossy().into_owned()), repo };
    }
    GitLocation::default()
}

fn real(path: &Path) -> String {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().into_owned()
}

/// Resolve a linked worktree's `.git` file (`gitdir: …`) to the shared git dir.
fn common_dir(dotgit_file: &Path) -> Option<String> {
    let content = fs::read(dotgit_file).ok()?;
    let content = String::from_utf8_lossy(&content);
    let gitdir = PathBuf::from(content.trim().strip_prefix("gitdir:")?.trim());
    let gitdir = dotgit_file.parent().unwrap().join(gitdir); // join keeps an absolute gitdir as is
    match fs::read_to_string(gitdir.join("commondir")) {
        Ok(common) => Some(real(&gitdir.join(common.trim()))),
        // Submodules have a gitdir but no commondir: the gitdir itself is the repository.
        Err(_) => Some(real(&gitdir)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// main checkout + linked worktree sharing one .git, and an unrelated directory.
    pub fn make_repo(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let main = tmp.join("repo");
        fs::create_dir_all(main.join(".git/worktrees/wt")).unwrap();
        fs::write(main.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
        fs::write(main.join(".git/worktrees/wt/HEAD"), "ref: refs/heads/feature\n").unwrap();
        fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir(main.join("src")).unwrap();
        let wt = tmp.join("wt");
        fs::create_dir(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", main.join(".git/worktrees/wt").display())).unwrap();
        let other = tmp.join("other");
        fs::create_dir(&other).unwrap();
        (main, wt, other)
    }

    #[test]
    fn locate_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let (main, wt, other) = make_repo(dir.path());
        let s = |p: &Path| p.to_string_lossy().into_owned();
        assert_eq!(locate(&s(&main.join("src"))).worktree, Some(s(&main)));
        assert_eq!(locate(&s(&wt)).worktree, Some(s(&wt)));
        assert_eq!(locate(&s(&wt)).repo, locate(&s(&main)).repo);
        assert!(locate(&s(&wt)).repo.is_some());
        assert_eq!(locate(&s(&other)).repo, None);
    }
}

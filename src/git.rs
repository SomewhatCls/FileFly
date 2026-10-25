/// This module provides functionality for interacting with Git repositories.
/// For this purpose, it implements commands via the shell.
use std::process::Command;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs;

/// Windows-specific Import and constant to hide the console window.
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Contains information about the current HEAD of a git repository.
/// This can either be a detached HEAD or a branch.
pub enum Head{
    Detached(String),
    Branch(String),
}

/// Enum representing different Git commands.
pub enum GitCommand {
    Checkout { branch: String },
    Branch { name: String, switch_to: bool },
    Add { paths: Vec<String> },
    Commit { message: String },
    Push { remote: String, branch: String, set_upstream: bool },
    Fetch { remote: Option<String> },
}

/// GitPaths caches which folders are git repositories.
/// It exists only for the duration of the current session.
pub struct GitPaths {
    cache: HashMap<PathBuf, Option<PathBuf>>,
}

impl GitPaths {
     pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Checks if a directory is part of a git repository.
    /// Returns the path to the root of the repository, if any.
    pub fn check_for_git(&mut self, dir: &Path) -> Option<PathBuf> {
        if let Some(hit) = self.cache.get(dir) {
            return hit.clone();
        }
        let result = if dir.join(".git").exists() {
            Some(dir.to_path_buf())
        } else {
            dir.parent().and_then(|p| self.check_for_git(p))
        };
        self.cache.insert(dir.to_path_buf(), result.clone());
        result
    }

    /// Used when a new git repo is initialized/cloned.
    pub fn refresh_cache(&mut self) {
        self.cache.clear();
    }
}

// Checks if git exists on the system.
#[cfg(target_os = "windows")]
pub fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
#[cfg(not(target_os = "windows"))]
pub fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns the current HEAD of a git repository.
pub fn get_head(dir: &Path) -> Option<Head>{
    let dot_git = dir.join(".git");
    let head = fs::read_to_string(dot_git.join("HEAD")).ok()?;
    let head = head.trim();

    if let Some(target) = head.strip_prefix("ref:") {
        let target = target.trim();
        let name = target.strip_prefix("refs/heads/").unwrap_or(target);
        Some(Head::Branch(name.to_string()))
    } else if !head.is_empty() {
        Some(Head::Detached(head.to_string()))
    } else {
        None
    }
}


/// Struct for branches and their upstream.
pub struct Branch {
    pub name: String,
    pub remote: Option<String>,
    pub tracking: Option<String>,
}

///  Returns the local branches of the current repository.
/// Also returns the remote and tracking branch
/// for each local branch, if applicable.
pub fn list_local_branches(repo: &Path) -> Result<Vec<Branch>, String> {
    let output = command(repo, &[
        "for-each-ref",
        "--format=%(refname:short)%09%(upstream:remotename)%09%(upstream:short)",
        "refs/heads",
    ])?;

    Ok(output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let name = parts.next()?.to_string();
            let remote = parts.next()?.to_string();        // "origin", or "" if none
            let tracking = parts.next()?.to_string();      // "origin/feature-x", or "" if none

            if remote.is_empty() {
                Some(Branch{ name, remote: None, tracking: None })
            } else {
                 Some(Branch{ name, remote: Some(remote), tracking: Some(tracking) })
            }
        })
        .collect())

}


/// Resolves the names of branch refs.
fn ref_names(repo: &Path, namespace: &str) -> Result<Vec<String>, String> {
    let out = command(repo, &["for-each-ref", "--format=%(refname:short)", namespace])?;
    Ok(out.lines().map(String::from).collect())
}

/// Passes commands to the git executable.
pub fn git(command: GitCommand, repo: &Path, args: Vec<String>) -> Result<String, String> {
    match command{
        GitCommand::Checkout { branch } => checkout(repo, &branch),
        GitCommand::Branch { name, switch_to } => branch(repo, &name, switch_to),
        GitCommand::Add { paths } => add(repo, &paths),
        GitCommand::Commit { message } => commit(repo, &message),
        GitCommand::Push { remote, branch, set_upstream } => push(repo, &remote, &branch, set_upstream),
        GitCommand::Fetch { remote } => fetch(repo, remote.as_deref()),
    }
}

///  Checks out the specified branch.
fn checkout(repo: &Path,  branch: &str) -> Result<String, String>{
    command(repo, &["checkout", branch])
}

/// Allows branch creation.
/// If `switch_to` is true, switches to the newly created branch.
fn branch(repo: &Path, name: &str, switch_to: bool) -> Result<String, String>{
    let mut args = vec!["branch"];
    if switch_to {
        command(repo, &["branch", "-b", name])
    }else{
        command(repo, &["branch", name])
    }
}

/// Adds files to the staging area.
fn add(repo: &Path, paths: &[String]) -> Result<String, String>{
    let mut args = vec!["add", "--"];
    args.extend(paths.iter().map(String::as_str));
    command(repo, &args)
}

///  Commits changes with the given message.
fn commit(repo: &Path, message: &str) -> Result<String, String>{
    command(repo, &["commit", "-m", message])
}

/// Pushes committed changes. If `set_upstream` is true, sets the upstream branch.
fn push(repo: &Path, remote: &str, branch: &str, set_upstream: bool) -> Result<String, String>{
    let mut args = vec!["push"];
    if set_upstream {
        args.push("-u");
    }
    args.push(remote);
    args.push(branch);
    command(repo, &args)
}

/// Fetches the latest changes from the remote repository.
/// If no remote is specified, fetches from the default remote.
fn fetch(repo: &Path, remote: Option<&str>) -> Result<String, String>{
    match remote {
        Some(r) => command(repo, &["fetch", r]),
        None => command(repo, &["fetch"]),
    }
}

fn command(repo: &Path,  args: &[&str]) -> Result<String, String>{
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}
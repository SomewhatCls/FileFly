/// This module provides functionality for interacting with Git repositories.
/// For this purpose, it implements commands via the shell.
use std::process::Command;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Windows-specific Import and constant to hide the console window.
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use crate::prelude::egui;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Contains information about the current HEAD of a git repository.
/// This can either be a detached HEAD or a branch.
#[derive(Clone, Debug, PartialEq)]
pub enum Head {
    Detached(String),
    Branch(String),
}


/// Enum representing different Git commands.
pub enum GitCommand {
    Checkout { branch: String },
    /*
    Branch { name: String, switch_to: bool },
    Add { paths: Vec<PathBuf> },
    Commit { message: String },
    Push { remote: String, branch: String, set_upstream: bool },
    Fetch { remote: Option<String> },*/
}

/// Struct for branches and their upstream.
#[derive(Clone, Debug)]
pub struct Branch {
    pub name: String,
    pub remote: Option<String>,
    pub tracking: Option<String>,
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

    // Used when a new git repo is initialized/cloned.
    //pub fn refresh_cache(&mut self) {
    //    self.cache.clear();
    //}
}


/// Records the current state of a git repository in a directory.
#[derive(Clone, Debug)]
pub struct GitSnapshot {
    pub root: PathBuf,
    pub head: Option<Head>,
    pub branches: Vec<Branch>,
    pub staged_files: usize,
}

/// Number of files staged for a commit.
pub fn staged_count(repo: &Path) -> usize {
    command(repo, &["diff", "--cached", "--name-only"])
        .map(|out| out.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

fn take_snapshot(root: &Path) -> GitSnapshot {
    GitSnapshot {
        root: root.to_path_buf(),
        head: get_head(root),
        branches: list_local_branches(root).unwrap_or_default(),
        staged_files: staged_count(root),
    }
}

/// Limits the frequency of local git repo existence checks.
const REFRESH_EVERY: Duration = Duration::from_secs(5);

/// Provides all the information the GUI needs regarding the current dirs git state.
pub struct GitState {
    paths: GitPaths,
    root: Option<PathBuf>,
    last_dir: Option<PathBuf>,
    last_refresh: Instant,
    pending: Option<Receiver<GitSnapshot>>,
    op: Option<Receiver<Result<String, String>>>,
    pub snapshot: Option<GitSnapshot>,
    pub error: Option<String>,
    /// Set after a successful checkout so the UI can reload the file listing.
    pub switched: bool,
    op_reloads: bool,
}

impl GitState {
    pub fn new() -> Self {
        Self {
            paths: GitPaths::new(),
            root: None,
            last_dir: None,
            last_refresh: Instant::now(),
            pending: None,
            op: None,
            snapshot: None,
            error: None,
            switched: false,
            op_reloads: false,
        }
    }

    pub fn busy(&self) -> bool {
        self.op.is_some()
    }

    /// Forcefully refreshes the git state.
    /*
    pub fn request_refresh(&mut self, ctx: &egui::Context) {
        self.pending = None; // Drops any occurring refresh.
        self.start_refresh(ctx);
    }*/

    /// Call once per frame unless the directory changed.
    pub fn update(&mut self, ctx: &egui::Context, dir: &Path) {
        // Finished background refresh?
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(snap) => {
                    // Ignore results for a repo we've already navigated away from.
                    if self.root.as_deref() == Some(snap.root.as_path()) {
                        self.snapshot = Some(snap);
                    }
                    self.last_refresh = Instant::now();
                    self.pending = None;
                }
                Err(TryRecvError::Disconnected) => self.pending = None,
                Err(TryRecvError::Empty) => {}
            }
        }

        // Checks checkout state.
        if let Some(rx) = &self.op {
            match rx.try_recv() {
                Ok(res) => {
                    self.switched = res.is_ok() && self.op_reloads;
                    self.error = res.err();
                    self.op = None;
                    self.pending = None;
                    self.start_refresh(ctx);
                }
                Err(TryRecvError::Disconnected) => self.op = None,
                Err(TryRecvError::Empty) => {}
            }
        }

        // Forcibly refreshes git repo existence if the directory changed.
        if self.last_dir.as_deref() != Some(dir) {
            self.last_dir = Some(dir.to_path_buf());
            let root = self.paths.check_for_git(dir);
            if root != self.root {
                self.root = root;
                self.snapshot = None;
                self.pending = None;
                self.error = None;
                self.start_refresh(ctx);
            }
        }

        // Picks up changes made outside FileFly.
        if self.root.is_some() {
            if self.pending.is_none() && self.op.is_none()
                && self.last_refresh.elapsed() > REFRESH_EVERY
            {
                self.start_refresh(ctx);
            }
            ctx.request_repaint_after(REFRESH_EVERY);
        }
    }

    fn start_refresh(&mut self, ctx: &egui::Context) {
        let Some(root) = self.root.clone() else { return };
        if self.pending.is_some() { return; }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(take_snapshot(&root));
            ctx.request_repaint(); // wake egui so it sees the result
        });
        self.pending = Some(rx);
    }

    pub fn git_async(&mut self, ctx: &egui::Context, command: GitCommand) {
        let Some(root) = self.root.clone() else { return };
        if self.op.is_some() { return; }
        self.op_reloads = matches!(command, GitCommand::Checkout { .. });
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let res = git(command, &root);
            let _ = tx.send(res);
            ctx.request_repaint();
        });
        self.op = Some(rx);
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
    if let Ok(res) = command(dir, &["symbolic-ref", "--short", "-q", "HEAD"]) {
        let head = res.trim();
        if !head.is_empty() {
            return Some(Head::Branch(head.to_string()));
        }
    }
    // Detached HEAD fallback
    command(dir, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(Head::Detached)
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

/// Parses commands for the git executable.
pub fn git(command: GitCommand, repo: &Path) -> Result<String, String> {
    match command{
        GitCommand::Checkout { branch } => checkout(repo, &branch),
        /*
        GitCommand::Branch { name, switch_to } => branch(repo, &name, switch_to),
        GitCommand::Add { paths } => add(repo, &paths),
        GitCommand::Commit { message } => commit(repo, &message),
        GitCommand::Push { remote, branch, set_upstream } => push(repo, &remote, &branch, set_upstream),
        GitCommand::Fetch { remote } => fetch(repo, remote.as_deref()),*/
    }
}

///  Checks out the specified branch.
fn checkout(repo: &Path,  branch: &str) -> Result<String, String>{
    command(repo, &["checkout", branch])
}

/// Allows branch creation.
/// If `switch_to` is true, switches to the newly created branch.
/*
fn branch(repo: &Path, name: &str, switch_to: bool) -> Result<String, String>{
    if switch_to {
        command(repo, &["switch", "-c", name])
    }else{
        command(repo, &["branch", name])
    }
}*/

/// Adds files to the staging area.
/*
fn add(repo: &Path, path_buf: &Vec<PathBuf>) -> Result<String, String>{
    let mut args = vec!["add", "--"];
    let paths: Vec<String> = path_buf
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    args.extend(paths.iter().map(String::as_str));
    command(repo, &args)
}*/

///  Commits changes with the given message.
/*
fn commit(repo: &Path, message: &str) -> Result<String, String>{
    command(repo, &["commit", "-m", message])
}*/

/// Pushes committed changes. If `set_upstream` is true, sets the upstream branch.
/*
fn push(repo: &Path, remote: &str, branch: &str, set_upstream: bool) -> Result<String, String>{
    let mut args = vec!["push"];
    if set_upstream {
        args.push("-u");
    }
    args.push(remote);
    args.push(branch);
    command(repo, &args)
}*/

/// Fetches the latest changes from the remote repository.
/// If no remote is specified, fetches from the default remote.
/*
fn fetch(repo: &Path, remote: Option<&str>) -> Result<String, String>{
    match remote {
        Some(r) => command(repo, &["fetch", r]),
        None => command(repo, &["fetch"]),
    }
}*/


/// Executes the given git command with args.
/// Throws an error for illegal commands.
#[cfg(target_os = "windows")]
fn command(dir: &Path,  args: &[&str]) -> Result<String, String>{
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(not(target_os = "windows"))]
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
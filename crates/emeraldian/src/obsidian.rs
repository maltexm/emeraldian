//! Integration with Obsidian's official command line interface.
//!
//! Obsidian ships a CLI (<https://obsidian.md/cli>) that drives the desktop
//! app. It is opt-in — the user enables it under Settings → General →
//! "Command line interface", which symlinks the binary onto their `PATH`.
//!
//! Two consequences shape this module:
//!
//! 1. The CLI may simply not be there, so every entry point degrades to a
//!    message explaining how to enable it rather than an error.
//! 2. It is a remote control for the app, not a backend: every command runs
//!    against an Obsidian instance, launching one if none is running.
//!    Everything emeraldian does to a vault it does by reading and writing
//!    Markdown directly; the CLI is only used for the things that need the app
//!    itself — listing the vaults it knows about, and opening a note in the GUI.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Why a CLI call could not be completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The binary was not found in any of the known locations.
    NotInstalled,
    /// The binary ran but reported a failure — most often "app not running".
    Failed(String),
    /// The binary could not be executed at all.
    Spawn(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(
                f,
                "the Obsidian CLI isn't on PATH; enable it in Obsidian under \
                 Settings → General → Command line interface"
            ),
            Self::Failed(message) => write!(f, "obsidian: {message}"),
            Self::Spawn(message) => write!(f, "could not run the Obsidian CLI: {message}"),
        }
    }
}

/// Locations the CLI is installed to when it is not on `PATH`, in the order the
/// docs describe them.
///
/// The in-bundle binary is the last resort, so a user who never enabled the
/// setting still gets a working "open in Obsidian" on macOS.
fn fallbacks() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/usr/local/bin/obsidian"),
        PathBuf::from("/opt/homebrew/bin/obsidian"),
    ];
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".local/bin/obsidian"));
    }
    paths.push(PathBuf::from(
        "/Applications/Obsidian.app/Contents/MacOS/obsidian-cli",
    ));
    paths
}

/// Finds the CLI.
///
/// `OBSIDIAN_CLI` decides first. Every command the CLI runs launches an
/// Obsidian window when none is open, so tests and scripted runs need a way to
/// guarantee that never happens: `off` (or an empty value) means "no CLI", and
/// a path means "exactly this binary". Only when the variable is unset does the
/// search go on to `PATH` and the usual install locations.
#[must_use]
pub fn locate() -> Option<PathBuf> {
    locate_with(
        std::env::var_os("OBSIDIAN_CLI").as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
}

/// [`locate`] for a given `OBSIDIAN_CLI` and `PATH`.
///
/// Taking them rather than reading them here is what lets a test check the
/// ordering without `set_var`. Writing to the environment from a test is
/// undefined behaviour, not merely untidy: the harness runs tests on several
/// threads, this module reads `OBSIDIAN_CLI` and `PATH`, and `setenv` may
/// reallocate the block `getenv` is walking.
fn locate_with(explicit: Option<&OsStr>, path: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        // An override that doesn't resolve is an answer, not a reason to keep
        // looking: falling through to `PATH` would reach the very binary the
        // override was set to avoid.
        if is_disabled(explicit) {
            return None;
        }
        return Some(PathBuf::from(explicit)).filter(|p| is_executable(p));
    }
    // `PATH` before the guesses: if the user enabled the setting, that symlink
    // is the one matching their installed app version.
    path.and_then(|path| on_path("obsidian", path))
        .or_else(|| fallbacks().into_iter().find(|p| is_executable(p)))
}

/// Whether an `OBSIDIAN_CLI` value switches the CLI off.
fn is_disabled(value: &OsStr) -> bool {
    let value = value.to_string_lossy();
    let value = value.trim();
    value.is_empty() || value.eq_ignore_ascii_case("off")
}

/// Searches a `PATH` value for an executable, the way a shell would.
fn on_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Runs a CLI subcommand and returns its stdout.
///
/// Arguments are passed through verbatim: the CLI's own syntax is
/// `obsidian <command> key=value`, and mirroring it keeps the slash command
/// honest about what it is really doing.
pub fn run(args: &[String]) -> Result<String, Error> {
    run_binary(locate(), args)
}

/// [`run`] against an already-located binary; `None` spawns nothing.
fn run_binary(binary: Option<PathBuf>, args: &[String]) -> Result<String, Error> {
    let binary = binary.ok_or(Error::NotInstalled)?;
    let output = Command::new(&binary)
        .args(args)
        .output()
        .map_err(|e| Error::Spawn(e.to_string()))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    // Whatever the CLI put on stderr is more useful than any wording we could
    // invent, so it is surfaced verbatim.
    let mut message = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if message.is_empty() {
        message = String::from_utf8_lossy(&output.stdout).trim().to_string();
    }
    if message.is_empty() {
        message = format!("exited with {}", output.status);
    }
    Err(Error::Failed(first_line(&message)))
}

/// The vaults Obsidian knows about.
pub fn vaults() -> Result<Vec<String>, Error> {
    let out = run(&["vaults".to_string()])?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Opens a vault-relative note in the Obsidian desktop app.
///
/// `path=` rather than `file=` because emeraldian always knows the exact
/// relative path, and `file=` would resolve by name and could land elsewhere.
pub fn open_note(vault: Option<&str>, rel: &str) -> Result<String, Error> {
    run(&open_args(vault, rel))?;
    Ok(format!("opened {rel} in Obsidian"))
}

fn open_args(vault: Option<&str>, rel: &str) -> Vec<String> {
    let mut args = vec!["open".to_string(), format!("path={rel}")];
    if let Some(vault) = vault {
        args.push(format!("vault={vault}"));
    }
    args
}

/// A one-line description of the integration's state, for `/obsidian` and the
/// command palette.
#[must_use]
pub fn status() -> String {
    match locate() {
        None => format!("Obsidian CLI: not found. {}", Error::NotInstalled),
        Some(path) => match vaults() {
            Ok(vaults) if vaults.is_empty() => {
                format!("Obsidian CLI: {} (no vaults reported)", path.display())
            }
            Ok(vaults) => format!(
                "Obsidian CLI: {} / vaults: {}",
                path.display(),
                vaults.join(", ")
            ),
            // The CLI starts Obsidian itself when nothing is running, so a
            // failure here isn't simply "the app is closed". Report what it
            // said rather than guessing at a cause.
            Err(err) => format!("Obsidian CLI: {} / {err}", path.display()),
        },
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_cli_explains_how_to_enable_it() {
        let message = Error::NotInstalled.to_string();
        assert!(message.contains("Settings"), "{message}");
        assert!(message.contains("Command line interface"), "{message}");
    }

    #[test]
    fn failures_are_reported_verbatim() {
        let err = Error::Failed("app is not running".into());
        assert!(err.to_string().contains("app is not running"));
    }

    /// A directory holding an executable named `name`, removed on drop.
    struct Fixture(PathBuf);

    impl Fixture {
        fn new(tag: &str, name: &str, body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir()
                .join(format!("emeraldian-obsidian-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("mkdir");
            let file = dir.join(name);
            std::fs::write(&file, body).expect("write");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            Self(dir)
        }

        fn dir(&self) -> &Path {
            &self.0
        }

        fn bin(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_bundled_binary_is_the_last_fallback() {
        let fallbacks = fallbacks();
        let last = fallbacks.last().expect("fallbacks are never empty");
        assert!(
            last.ends_with("obsidian-cli"),
            "the in-bundle binary is a fallback, not a preference: {last:?}"
        );
    }

    #[test]
    fn an_explicit_override_beats_path() {
        // `OBSIDIAN_CLI` is the escape hatch, so it has to beat a binary that
        // is already on `PATH`, not merely be tried after it.
        let on_path = Fixture::new("beats-path-a", "obsidian", "#!/bin/sh\n");
        let chosen = Fixture::new("beats-path-b", "stub", "#!/bin/sh\n");
        let found = locate_with(
            Some(chosen.bin("stub").as_os_str()),
            Some(on_path.dir().as_os_str()),
        );
        assert_eq!(found, Some(chosen.bin("stub")));
    }

    #[test]
    fn an_unresolvable_override_does_not_fall_back_to_path() {
        // Falling through would reach the real binary the override was set to
        // avoid.
        let on_path = Fixture::new("no-fallthrough", "obsidian", "#!/bin/sh\n");
        let found = locate_with(
            Some(OsStr::new("/nonexistent/obsidian-stub")),
            Some(on_path.dir().as_os_str()),
        );
        assert_eq!(found, None);
    }

    #[test]
    fn without_an_override_path_is_searched() {
        let on_path = Fixture::new("path-search", "obsidian", "#!/bin/sh\n");
        let found = locate_with(None, Some(on_path.dir().as_os_str()));
        assert_eq!(found, Some(on_path.bin("obsidian")));
    }

    #[test]
    fn off_and_empty_disable_the_cli_even_with_one_on_path() {
        let on_path = Fixture::new("off-locate", "obsidian", "#!/bin/sh\n");
        for value in ["off", "OFF", "", "  "] {
            assert_eq!(
                locate_with(Some(OsStr::new(value)), Some(on_path.dir().as_os_str())),
                None,
                "OBSIDIAN_CLI={value:?}"
            );
        }
    }

    #[test]
    fn off_never_spawns() {
        // The binary on `PATH` leaves a marker if it is ever executed.
        let marker = std::env::temp_dir().join(format!(
            "emeraldian-obsidian-spawned-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&marker);
        let on_path = Fixture::new(
            "off-spawn",
            "obsidian",
            &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        );

        let located = locate_with(Some(OsStr::new("off")), Some(on_path.dir().as_os_str()));
        let result = run_binary(located, &["vaults".to_string()]);

        assert_eq!(result, Err(Error::NotInstalled));
        assert!(!marker.exists(), "the stub was executed");
    }

    #[test]
    fn only_the_first_line_of_an_error_is_kept() {
        assert_eq!(first_line("bad thing\nstack trace\nmore"), "bad thing");
    }

    #[test]
    fn a_directory_is_not_executable() {
        assert!(!is_executable(Path::new("/tmp")));
    }

    #[test]
    fn open_targets_the_exact_path_not_the_name() {
        // Guards the `path=` vs `file=` choice: `file=` resolves by name and
        // could open a different note that happens to share a title.
        assert_eq!(
            open_args(None, "Folder/A.md"),
            vec!["open".to_string(), "path=Folder/A.md".to_string()]
        );
    }

    #[test]
    fn open_can_target_a_named_vault() {
        assert_eq!(
            open_args(Some("Holocron"), "A.md"),
            vec![
                "open".to_string(),
                "path=A.md".to_string(),
                "vault=Holocron".to_string()
            ]
        );
    }
}

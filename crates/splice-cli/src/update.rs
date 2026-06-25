//! Self-update and uninstall for the `splice` binary.
//!
//! * [`auto_check`] runs on every command (unless opted out) and offers to
//!   install a newer release interactively.
//! * [`cmd_update`] force-installs the latest release (`splice update`).
//! * [`cmd_uninstall`] removes the binary + the PATH lines `install.sh` added,
//!   deferring to the package manager for npm/cargo installs (`splice uninstall`).
//!
//! Updates fetch the matching prebuilt asset from GitHub Releases and atomically
//! replace the running executable in place — the same assets `install.sh` ships.

use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

const LATEST_API: &str = "https://api.github.com/repos/Pogo-Bash/codonsplice/releases/latest";
const RELEASE_DL: &str = "https://github.com/Pogo-Bash/codonsplice/releases/download";
const NPM_PKG: &str = "@codonsplice/cli";

/// The version this binary was compiled as.
fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// How `splice` got onto this machine, inferred from the executable's path.
enum InstallKind {
    /// Inside a `node_modules` tree — npm owns the binary.
    Npm,
    /// Under a cargo bin dir (`cargo install`).
    Cargo,
    /// A plain prebuilt binary (install.sh, manual download, etc).
    Binary,
}

fn detect_install(exe: &Path) -> InstallKind {
    let p = exe.to_string_lossy();
    if p.contains("node_modules") {
        InstallKind::Npm
    } else if p.contains(".cargo") {
        InstallKind::Cargo
    } else {
        InstallKind::Binary
    }
}

// ── Version handling ─────────────────────────────────────────────────────────

/// Parse `1.2.3` (or `v1.2.3`, `1.2.3-rc1`) into a comparable triple.
fn parse_ver(s: &str) -> (u64, u64, u64) {
    let core = s.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut parts = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn is_newer(latest: &str, current: &str) -> bool {
    parse_ver(latest) > parse_ver(current)
}

/// Fetch the latest release tag (without the leading `v`). `None` if offline.
fn fetch_latest_version() -> Option<String> {
    let resp = ureq::get(LATEST_API)
        .set("User-Agent", "splice-cli")
        .timeout(Duration::from_secs(3))
        .call()
        .ok()?;
    let body = resp.into_string().ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = json.get("tag_name").and_then(|v| v.as_str())?;
    Some(tag.trim_start_matches('v').to_string())
}

// ── Asset download + install ─────────────────────────────────────────────────

/// Release asset base name for the host platform (matches install.sh / CI).
/// `None` on a platform we don't publish binaries for.
fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("splice-linux-x86_64"),
        ("linux", "aarch64") => Some("splice-linux-aarch64"),
        ("macos", "x86_64") => Some("splice-macos-x86_64"),
        ("windows", "x86_64") => Some("splice-windows-x86_64.exe"),
        _ => None,
    }
}

fn ioerr(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg.into())
}

/// Download the release asset bytes for `version` and the host platform.
fn download_asset(version: &str) -> io::Result<Vec<u8>> {
    let base = asset_name().ok_or_else(|| {
        ioerr(format!(
            "no prebuilt binary for {}-{} — reinstall from source (cargo install splice-cli)",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;
    // Windows ships the raw .exe; unix ships a gzipped tarball of `splice`.
    let file = if base.ends_with(".exe") {
        base.to_string()
    } else {
        format!("{base}.tar.gz")
    };
    let url = format!("{RELEASE_DL}/v{version}/{file}");

    let resp = ureq::get(&url)
        .set("User-Agent", "splice-cli")
        .timeout(Duration::from_secs(60))
        .call()
        .map_err(|e| ioerr(format!("downloading {url}: {e}")))?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

/// Turn the downloaded asset into the raw `splice` executable bytes.
fn extract_binary(asset: &[u8]) -> io::Result<Vec<u8>> {
    // Windows asset is the executable itself.
    if cfg!(windows) {
        return Ok(asset.to_vec());
    }
    // Unix: `tar xz` the `splice` member out of the gzipped tarball.
    let tmp = std::env::temp_dir().join(format!("splice-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let tarball = tmp.join("asset.tar.gz");
    std::fs::write(&tarball, asset)?;
    let status = Command::new("tar")
        .arg("xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&tmp)
        .arg("splice")
        .status()
        .map_err(|e| ioerr(format!("running tar: {e} (is `tar` installed?)")))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(ioerr("tar failed to extract `splice` from the release asset"));
    }
    let bytes = std::fs::read(tmp.join("splice"))?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(bytes)
}

/// Atomically replace the currently-running executable with `new_bytes`.
fn replace_current_exe(new_bytes: &[u8]) -> io::Result<PathBuf> {
    let cur = std::env::current_exe()?;
    let dir = cur.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".splice-update-{}", std::process::id()));

    // Stage the new binary next to the target so the rename stays on one device.
    std::fs::write(&tmp, new_bytes).map_err(|e| {
        ioerr(format!(
            "cannot write to {} ({e}) — you may need elevated permissions",
            dir.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(windows)]
    {
        // A running .exe can't be overwritten; move it aside first (deleted next run).
        let old = cur.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&cur, &old)?;
    }
    std::fs::rename(&tmp, &cur).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        ioerr(format!(
            "cannot replace {} ({e}) — you may need elevated permissions",
            cur.display()
        ))
    })?;
    Ok(cur)
}

/// Download + install `version`, returning the path that was updated.
/// Refuses to self-replace an npm-managed binary (npm would clobber it).
fn install_version(version: &str) -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if let InstallKind::Npm = detect_install(&exe) {
        return Err(ioerr(format!(
            "this splice is managed by npm — update with: npm update -g {NPM_PKG}"
        )));
    }
    let asset = download_asset(version)?;
    let bin = extract_binary(&asset)?;
    replace_current_exe(&bin)
}

// ── Public entry points ──────────────────────────────────────────────────────

/// Best-effort update check run before normal commands. Never aborts the
/// command on failure; only prompts to install when a newer release is found.
pub fn auto_check(no_update: bool) {
    if no_update || std::env::var_os("SPLICE_NO_UPDATE").is_some() {
        return;
    }
    let cur = current_version();
    let latest = match fetch_latest_version() {
        Some(v) => v,
        None => return, // offline / rate-limited — stay quiet
    };
    if !is_newer(&latest, cur) {
        return;
    }

    eprintln!("splice: v{latest} available (you have {cur}).");

    let exe = std::env::current_exe().unwrap_or_default();
    if let InstallKind::Npm = detect_install(&exe) {
        eprintln!("  update with: npm update -g {NPM_PKG}");
        return;
    }

    // Only prompt when attached to a terminal; otherwise just hint and move on
    // so scripts and pipelines are never blocked.
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        eprintln!("  run `splice update` to install.");
        return;
    }

    eprint!("Update now? [y/N] ");
    let _ = io::stderr().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return;
    }
    let ans = line.trim().to_ascii_lowercase();
    if ans != "y" && ans != "yes" {
        return;
    }

    match install_version(&latest) {
        Ok(path) => eprintln!(
            "✓ updated to {latest} ({}). Re-run your command to use the new version.",
            path.display()
        ),
        Err(e) => eprintln!("✗ update failed: {e}"),
    }
}

/// `splice update` — force-install the latest release.
pub fn cmd_update() -> ExitCode {
    let cur = current_version();
    eprintln!("splice {cur}: checking for updates…");
    let latest = match fetch_latest_version() {
        Some(v) => v,
        None => {
            eprintln!("✗ could not reach the release server (offline or rate-limited).");
            return ExitCode::FAILURE;
        }
    };
    if !is_newer(&latest, cur) {
        println!("✓ splice is already up to date ({cur}).");
        return ExitCode::SUCCESS;
    }
    println!("→ updating splice {cur} → {latest}…");
    match install_version(&latest) {
        Ok(path) => {
            println!("✓ updated to {latest} at {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("✗ update failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `splice uninstall` — remove the binary and PATH entries, or defer to the
/// package manager that owns it.
pub fn cmd_uninstall() -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("✗ cannot locate the splice binary: {e}");
            return ExitCode::FAILURE;
        }
    };

    match detect_install(&exe) {
        InstallKind::Npm => {
            println!("This splice is managed by npm — remove it with:");
            println!("  npm uninstall -g {NPM_PKG}");
            return ExitCode::SUCCESS;
        }
        InstallKind::Cargo => {
            println!("This splice looks cargo-installed ({}).", exe.display());
            println!("If you used `cargo install`, the clean removal is:");
            println!("  cargo uninstall splice-cli");
            println!();
        }
        InstallKind::Binary => {}
    }

    // Confirm before deleting anything.
    eprint!("Remove splice at {}? [y/N] ", exe.display());
    let _ = io::stderr().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return ExitCode::FAILURE;
    }
    let ans = line.trim().to_ascii_lowercase();
    if ans != "y" && ans != "yes" {
        println!("aborted — nothing was removed.");
        return ExitCode::SUCCESS;
    }

    if let Err(e) = std::fs::remove_file(&exe) {
        eprintln!(
            "✗ could not remove {} ({e}) — you may need elevated permissions.",
            exe.display()
        );
        return ExitCode::FAILURE;
    }
    println!("✓ removed {}", exe.display());

    // Strip the `# CodonSplice` / `export PATH=…` block install.sh appended.
    if let Some(dir) = exe.parent() {
        let removed = clean_path_entries(dir);
        for rc in &removed {
            println!("✓ cleaned PATH entry in {}", rc.display());
        }
    }

    println!("Done. Open a new shell to refresh your PATH.");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(parse_ver("0.1.2"), (0, 1, 2));
        assert_eq!(parse_ver("v0.1.2"), (0, 1, 2));
        assert_eq!(parse_ver("1.20.3-rc1"), (1, 20, 3));
        assert_eq!(parse_ver("2"), (2, 0, 0));
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.1.2", "0.1.1"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.2", "0.1.2"));
        assert!(!is_newer("0.1.1", "0.1.2"));
        // Numeric, not lexicographic: 0.1.10 > 0.1.9.
        assert!(is_newer("0.1.10", "0.1.9"));
    }
}

/// Remove the two-line block install.sh adds to shell rc files:
///
/// ```text
/// # CodonSplice
/// export PATH="<dir>:$PATH"
/// ```
///
/// Returns the rc files that were modified.
fn clean_path_entries(install_dir: &Path) -> Vec<PathBuf> {
    let home = match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h),
        None => return Vec::new(),
    };
    let export_line = format!("export PATH=\"{}:$PATH\"", install_dir.display());
    let mut modified = Vec::new();

    for name in [".bashrc", ".zshrc", ".bash_profile", ".profile"] {
        let rc = home.join(name);
        let Ok(content) = std::fs::read_to_string(&rc) else {
            continue;
        };
        let mut out = Vec::new();
        let mut changed = false;
        for line in content.lines() {
            let t = line.trim();
            if t == "# CodonSplice" || t == export_line {
                changed = true;
                continue;
            }
            out.push(line);
        }
        if changed {
            // Preserve a trailing newline; collapse the gap the block left behind.
            let mut joined = out.join("\n");
            if content.ends_with('\n') {
                joined.push('\n');
            }
            if std::fs::write(&rc, joined).is_ok() {
                modified.push(rc);
            }
        }
    }
    modified
}

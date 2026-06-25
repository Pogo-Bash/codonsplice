//! The `splice install` guided TUI installer.
//!
//! Walks the user through a five-step flow — welcome → environment detection →
//! install method → live install progress → post-install verification —
//! detecting the toolchain via real subprocesses, fetching the latest release
//! from the GitHub API (via `ureq`), and streaming install output line-by-line
//! through an `mpsc` channel so the TUI stays responsive. Cancellable any time
//! with `q` or `Ctrl+C`.

use std::io;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Wrap},
    DefaultTerminal, Frame,
};

// ── Catppuccin Mocha palette ─────────────────────────────────────────────────
const GREEN: Color = Color::Rgb(166, 227, 161);
const RED: Color = Color::Rgb(243, 139, 168);
const PEACH: Color = Color::Rgb(250, 179, 135);
const MAUVE: Color = Color::Rgb(203, 166, 247);
const TEXT: Color = Color::Rgb(205, 214, 244);
const SUBTLE: Color = Color::Rgb(127, 132, 156);

const GITHUB_LATEST: &str =
    "https://api.github.com/repos/Pogo-Bash/codonsplice/releases/latest";

#[derive(PartialEq, Clone, Copy)]
enum Step {
    Welcome,
    Detect,
    Method,
    Progress,
    Verify,
}

#[derive(Clone, Copy, PartialEq)]
enum Choice {
    CliTools,
    RustLib,
    Npm,
    All,
}

impl Choice {
    fn label(self) -> &'static str {
        match self {
            Choice::CliTools => "Install CLI tools (splice binary)",
            Choice::RustLib => "Install as Rust library (Cargo.toml)",
            Choice::Npm => "Install npm package (@codonsplice/wasm)",
            Choice::All => "Install all",
        }
    }
}

const WELCOME_MENU: [Choice; 4] = [Choice::CliTools, Choice::RustLib, Choice::Npm, Choice::All];

#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)] // Pending is a valid status the renderer handles, set in async variants
enum CheckStatus {
    Pending,
    Ok,
    Missing,
}

struct EnvCheck {
    label: String,
    status: CheckStatus,
    detail: String,
    optional: bool,
}

/// One npm package toggle (name, selected).
struct NpmPkg {
    name: &'static str,
    selected: bool,
}

enum InstallMsg {
    Line(String),
    Done(bool),
}

struct InstallerState {
    step: Step,
    menu_idx: usize,
    choice: Option<Choice>,
    checks: Vec<EnvCheck>,
    detected: bool,
    latest_version: Option<String>,
    npm_pkgs: Vec<NpmPkg>,
    npm_idx: usize,
    output: Vec<String>,
    rx: Option<mpsc::Receiver<InstallMsg>>,
    install_done: bool,
    install_ok: bool,
    progress: u16,
    verify_lines: Vec<(String, bool)>,
    quit: bool,
}

impl InstallerState {
    fn new() -> Self {
        InstallerState {
            step: Step::Welcome,
            menu_idx: 0,
            choice: None,
            checks: Vec::new(),
            detected: false,
            latest_version: None,
            npm_pkgs: vec![
                NpmPkg { name: "@codonsplice/wasm", selected: true },
                NpmPkg { name: "@codonsplice/react", selected: false },
                NpmPkg { name: "@codonsplice/vue", selected: false },
                NpmPkg { name: "@codonsplice/svelte", selected: false },
                NpmPkg { name: "@codonsplice/astro", selected: false },
            ],
            npm_idx: 0,
            output: Vec::new(),
            rx: None,
            install_done: false,
            install_ok: false,
            progress: 0,
            verify_lines: Vec::new(),
            quit: false,
        }
    }
}

/// Public entry point: `splice install`.
pub fn run() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal);
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut st = InstallerState::new();
    // Kick off the (best-effort) latest-version fetch immediately.
    st.latest_version = fetch_latest_version();

    while !st.quit {
        terminal.draw(|frame| draw(frame, &st))?;
        drain_install(&mut st);

        if event::poll(Duration::from_millis(120))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        st.quit = true;
                        continue;
                    }
                    on_key(&mut st, key.code);
                }
            }
        }
    }
    Ok(())
}

// ── Input handling ───────────────────────────────────────────────────────────

fn on_key(st: &mut InstallerState, code: KeyCode) {
    match st.step {
        Step::Welcome => match code {
            KeyCode::Char('q') | KeyCode::Char('Q') => st.quit = true,
            KeyCode::Up => st.menu_idx = st.menu_idx.saturating_sub(1),
            KeyCode::Down => st.menu_idx = (st.menu_idx + 1).min(WELCOME_MENU.len() - 1),
            KeyCode::Char(c @ '1'..='4') => {
                st.menu_idx = (c as usize) - ('1' as usize);
                enter_detect(st);
            }
            KeyCode::Enter => enter_detect(st),
            _ => {}
        },
        Step::Detect => match code {
            KeyCode::Char('q') | KeyCode::Char('Q') => st.quit = true,
            KeyCode::Enter => st.step = Step::Method,
            _ => {}
        },
        Step::Method => match code {
            KeyCode::Char('q') | KeyCode::Char('Q') => st.quit = true,
            KeyCode::Up => {
                if is_npm(st) {
                    st.npm_idx = st.npm_idx.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if is_npm(st) {
                    st.npm_idx = (st.npm_idx + 1).min(st.npm_pkgs.len() - 1);
                }
            }
            KeyCode::Char(' ') => {
                if is_npm(st) {
                    let i = st.npm_idx;
                    st.npm_pkgs[i].selected = !st.npm_pkgs[i].selected;
                }
            }
            KeyCode::Enter => start_install(st),
            _ => {}
        },
        Step::Progress => match code {
            KeyCode::Char('q') | KeyCode::Char('Q') => st.quit = true,
            KeyCode::Enter if st.install_done => enter_verify(st),
            KeyCode::Char('r') if st.install_done && !st.install_ok => start_install(st),
            _ => {}
        },
        Step::Verify => match code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Enter => st.quit = true,
            _ => {}
        },
    }
}

fn is_npm(st: &InstallerState) -> bool {
    matches!(st.choice, Some(Choice::Npm) | Some(Choice::All))
}

fn enter_detect(st: &mut InstallerState) {
    st.choice = Some(WELCOME_MENU[st.menu_idx]);
    st.step = Step::Detect;
    if !st.detected {
        st.checks = detect_environment();
        st.detected = true;
    }
}

fn enter_verify(st: &mut InstallerState) {
    st.step = Step::Verify;
    st.verify_lines = run_verification(st);
}

// ── Step 2: environment detection (real subprocesses) ────────────────────────

fn detect_environment() -> Vec<EnvCheck> {
    let mut checks = Vec::new();

    checks.push(EnvCheck {
        label: "OS".to_string(),
        status: CheckStatus::Ok,
        detail: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        optional: false,
    });

    for (label, cmd, optional) in [
        ("rustc", "rustc", false),
        ("cargo", "cargo", false),
        ("node", "node", true),
        ("npm", "npm", true),
        ("wasm-pack", "wasm-pack", true),
    ] {
        let (status, detail) = match Command::new(cmd).arg("--version").output() {
            Ok(out) if out.status.success() => {
                let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
                (CheckStatus::Ok, v)
            }
            _ => (CheckStatus::Missing, "not found".to_string()),
        };
        checks.push(EnvCheck {
            label: label.to_string(),
            status,
            detail,
            optional,
        });
    }
    checks
}

// ── GitHub releases ──────────────────────────────────────────────────────────

fn fetch_latest_version() -> Option<String> {
    let resp = ureq::get(GITHUB_LATEST)
        .set("User-Agent", "codonsplice-installer")
        .timeout(Duration::from_secs(4))
        .call()
        .ok()?;
    let body = resp.into_string().ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    json.get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ── Step 4: install (streamed subprocess) ────────────────────────────────────

/// The shell command(s) to run for the chosen install method.
fn install_commands(st: &InstallerState) -> Vec<(String, Vec<String>)> {
    let mut cmds = Vec::new();
    let choice = st.choice.unwrap_or(Choice::CliTools);

    let want_cli = matches!(choice, Choice::CliTools | Choice::All);
    let want_npm = matches!(choice, Choice::Npm | Choice::All);

    if want_cli {
        cmds.push(("cargo".into(), vec!["install".into(), "codonsplice".into()]));
    }
    if want_npm {
        for pkg in st.npm_pkgs.iter().filter(|p| p.selected) {
            cmds.push(("npm".into(), vec!["install".into(), pkg.name.to_string()]));
        }
    }
    // RustLib is informational only (no command); handled in the Method view.
    cmds
}

fn start_install(st: &mut InstallerState) {
    // RustLib: nothing to run, go straight to verification.
    if st.choice == Some(Choice::RustLib) {
        enter_verify(st);
        return;
    }

    let cmds = install_commands(st);
    if cmds.is_empty() {
        enter_verify(st);
        return;
    }

    st.step = Step::Progress;
    st.output.clear();
    st.install_done = false;
    st.install_ok = false;
    st.progress = 0;

    let (tx, rx) = mpsc::channel::<InstallMsg>();
    st.rx = Some(rx);

    thread::spawn(move || {
        let mut all_ok = true;
        for (cmd, args) in cmds {
            let _ = tx.send(InstallMsg::Line(format!("> {} {}", cmd, args.join(" "))));
            let ok = run_streamed(&cmd, &args, &tx);
            all_ok = all_ok && ok;
            if !ok {
                let _ = tx.send(InstallMsg::Line(format!("  ✗ `{cmd}` failed")));
                break;
            }
        }
        let _ = tx.send(InstallMsg::Done(all_ok));
    });
}

/// Run a command, forwarding merged stdout/stderr lines to `tx`. Returns success.
fn run_streamed(cmd: &str, args: &[String], tx: &mpsc::Sender<InstallMsg>) -> bool {
    use std::io::{BufRead, BufReader};

    let mut child = match Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(InstallMsg::Line(format!("  error: {e}")));
            return false;
        }
    };

    let mut handles = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(InstallMsg::Line(format!("  {line}")));
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let _ = tx.send(InstallMsg::Line(format!("  {line}")));
            }
        }));
    }

    let status = child.wait();
    for h in handles {
        let _ = h.join();
    }
    matches!(status, Ok(s) if s.success())
}

/// Drain any pending install output into the state, advancing the progress bar.
fn drain_install(st: &mut InstallerState) {
    let mut done = None;
    if let Some(rx) = &st.rx {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                InstallMsg::Line(l) => {
                    st.output.push(l);
                    if st.progress < 95 {
                        st.progress += 2;
                    }
                }
                InstallMsg::Done(ok) => done = Some(ok),
            }
        }
    }
    if let Some(ok) = done {
        st.install_done = true;
        st.install_ok = ok;
        st.progress = 100;
        st.rx = None;
    }
}

// ── Step 5: verification ─────────────────────────────────────────────────────

fn run_verification(st: &InstallerState) -> Vec<(String, bool)> {
    let mut lines = Vec::new();
    let choice = st.choice.unwrap_or(Choice::CliTools);

    if matches!(choice, Choice::CliTools | Choice::All) {
        match Command::new("splice").arg("--version").output() {
            Ok(out) if out.status.success() => lines.push((
                format!("splice {}", String::from_utf8_lossy(&out.stdout).trim()),
                true,
            )),
            _ => lines.push(("splice not on PATH (open a new shell?)".into(), false)),
        }
    }
    if matches!(choice, Choice::Npm | Choice::All) {
        let ok = Command::new("node")
            .args(["-e", "require('@codonsplice/wasm')"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        lines.push((
            if ok {
                "@codonsplice/wasm available in Node".into()
            } else {
                "@codonsplice/wasm not importable".into()
            },
            ok,
        ));
    }
    if matches!(choice, Choice::RustLib) {
        lines.push((
            "add `codonsplice = \"0.1\"` + `spliceql = \"0.1\"` to Cargo.toml".into(),
            true,
        ));
    }
    lines
}

// ── Rendering ────────────────────────────────────────────────────────────────

fn draw(frame: &mut Frame, st: &InstallerState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header / tabs
            Constraint::Min(5),    // body
            Constraint::Length(2), // footer
        ])
        .split(area);

    draw_header(frame, chunks[0], st);
    draw_body(frame, chunks[1], st);
    draw_footer(frame, chunks[2], st);
}

fn draw_header(frame: &mut Frame, area: Rect, st: &InstallerState) {
    let steps = ["WELCOME", "DETECT", "METHOD", "INSTALL", "VERIFY"];
    let active = match st.step {
        Step::Welcome => 0,
        Step::Detect => 1,
        Step::Method => 2,
        Step::Progress => 3,
        Step::Verify => 4,
    };
    let mut spans = vec![Span::styled(
        " CodonSplice Installer  ",
        Style::new().fg(MAUVE).add_modifier(Modifier::BOLD),
    )];
    for (i, s) in steps.iter().enumerate() {
        let style = if i == active {
            Style::new().fg(Color::Black).bg(PEACH).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(SUBTLE)
        };
        spans.push(Span::styled(format!(" {s} "), style));
        spans.push(Span::raw(" "));
    }
    let p = Paragraph::new(Line::from(spans))
        .block(Block::default().borders(Borders::ALL).border_style(Style::new().fg(SUBTLE)));
    frame.render_widget(p, area);
}

fn draw_body(frame: &mut Frame, area: Rect, st: &InstallerState) {
    let lines = match st.step {
        Step::Welcome => body_welcome(st),
        Step::Detect => body_detect(st),
        Step::Method => body_method(st),
        Step::Progress => body_progress(frame, area, st),
        Step::Verify => body_verify(st),
    };
    if lines.is_empty() {
        return; // progress draws itself
    }
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).border_style(Style::new().fg(SUBTLE)));
    frame.render_widget(p, area);
}

fn body_welcome(st: &InstallerState) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            "  ██████╗ ██████╗ ██████╗  ██████╗ ███╗   ██╗",
            Style::new().fg(MAUVE),
        )),
        Line::from(Span::styled(
            "  CodonSplice — install spliceql + codonsplice-core",
            Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            match &st.latest_version {
                Some(v) => format!("  latest release: {v}"),
                None => "  latest release: (offline / unpublished)".to_string(),
            },
            Style::new().fg(SUBTLE),
        )),
        Line::raw(""),
    ];
    for (i, choice) in WELCOME_MENU.iter().enumerate() {
        let marker = if i == st.menu_idx { "▶ " } else { "  " };
        let style = if i == st.menu_idx {
            Style::new().fg(GREEN).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(TEXT)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}[{}] {}", i + 1, choice.label()),
            style,
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  [Q] Quit installer",
        Style::new().fg(SUBTLE),
    )));
    lines
}

fn body_detect(st: &InstallerState) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "Detecting environment…",
        Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
    )), Line::raw("")];
    for c in &st.checks {
        let (sym, color) = match c.status {
            CheckStatus::Ok => ("✓", GREEN),
            CheckStatus::Missing if c.optional => ("→", PEACH),
            CheckStatus::Missing => ("✗", RED),
            CheckStatus::Pending => ("·", SUBTLE),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {sym} "), Style::new().fg(color)),
            Span::styled(format!("{:<12}", c.label), Style::new().fg(TEXT)),
            Span::styled(c.detail.clone(), Style::new().fg(SUBTLE)),
        ]));
    }
    if st.checks.iter().any(|c| c.label == "wasm-pack" && c.status == CheckStatus::Missing) {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "  → wasm-pack missing — required for WASM builds: cargo install wasm-pack",
            Style::new().fg(PEACH),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Press Enter to choose an install method.",
        Style::new().fg(SUBTLE),
    )));
    lines
}

fn body_method(st: &InstallerState) -> Vec<Line<'static>> {
    let choice = st.choice.unwrap_or(Choice::CliTools);
    let mut lines = Vec::new();
    match choice {
        Choice::CliTools | Choice::All if choice == Choice::CliTools => {
            lines.push(header_line("CLI tools"));
            lines.push(cmd_line("Recommended:  cargo install codonsplice"));
            lines.push(plain(format!(
                "  Prebuilt binary for this host: {}",
                prebuilt_asset()
            )));
            lines.push(plain("  Press Enter to run the recommended command."));
        }
        Choice::RustLib => {
            lines.push(header_line("Rust library"));
            lines.push(cmd_line("# Add to your Cargo.toml:"));
            lines.push(cmd_line("codonsplice = \"0.1\""));
            lines.push(cmd_line("spliceql    = \"0.1\""));
            lines.push(plain("  Press Enter to continue (no build step here)."));
        }
        Choice::Npm | Choice::All => {
            lines.push(header_line("npm packages — Space to toggle, Enter to install"));
            for (i, pkg) in st.npm_pkgs.iter().enumerate() {
                let box_ = if pkg.selected { "[x]" } else { "[ ]" };
                let marker = if i == st.npm_idx { "▶ " } else { "  " };
                let style = if i == st.npm_idx {
                    Style::new().fg(GREEN)
                } else {
                    Style::new().fg(TEXT)
                };
                lines.push(Line::from(Span::styled(
                    format!("{marker}{box_} npm install {}", pkg.name),
                    style,
                )));
            }
            if choice == Choice::All {
                lines.push(Line::raw(""));
                lines.push(plain("  (CLI tools will also be installed via cargo.)"));
            }
        }
        _ => {}
    }
    lines
}

fn body_progress(frame: &mut Frame, area: Rect, st: &InstallerState) -> Vec<Line<'static>> {
    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3)])
        .split(area);

    let (color, label) = if st.install_done {
        if st.install_ok {
            (GREEN, "✓ installation complete".to_string())
        } else {
            (RED, "✗ installation failed".to_string())
        }
    } else {
        (PEACH, "installing…".to_string())
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(label))
        .gauge_style(Style::new().fg(color))
        .percent(st.progress);
    frame.render_widget(gauge, inner[0]);

    let start = st.output.len().saturating_sub((inner[1].height as usize).saturating_sub(2));
    let lines: Vec<Line> = st.output[start..]
        .iter()
        .map(|l| {
            let style = if l.contains('✗') || l.to_lowercase().contains("error") {
                Style::new().fg(RED)
            } else if l.starts_with('>') {
                Style::new().fg(MAUVE)
            } else {
                Style::new().fg(TEXT)
            };
            Line::from(Span::styled(l.clone(), style))
        })
        .collect();
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("output"));
    frame.render_widget(p, inner[1]);
    Vec::new()
}

fn body_verify(st: &InstallerState) -> Vec<Line<'static>> {
    let mut lines = vec![header_line("Verification"), Line::raw("")];
    for (msg, ok) in &st.verify_lines {
        let (sym, color) = if *ok { ("✓", GREEN) } else { ("✗", RED) };
        lines.push(Line::from(vec![
            Span::styled(format!("  {sym} "), Style::new().fg(color)),
            Span::styled(msg.clone(), Style::new().fg(TEXT)),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(plain("  Next: `splice` opens the query editor, or try the sample query:"));
    lines.push(cmd_line("    FROM bam \"sample.bam\" WHERE depth > 30 CALL variants WITH min_af = 0.05"));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Press Enter or Q to exit.",
        Style::new().fg(SUBTLE),
    )));
    lines
}

fn draw_footer(frame: &mut Frame, area: Rect, st: &InstallerState) {
    let hint = match st.step {
        Step::Welcome => "↑/↓ select · 1-4 quick pick · Enter continue · Q quit",
        Step::Detect => "Enter continue · Q quit · Ctrl+C cancel",
        Step::Method => "↑/↓ move · Space toggle · Enter install · Q quit",
        Step::Progress => {
            if st.install_done && !st.install_ok {
                "R retry · Enter verify · Q quit"
            } else if st.install_done {
                "Enter verify · Q quit"
            } else {
                "installing… · Ctrl+C cancel"
            }
        }
        Step::Verify => "Enter/Q exit",
    };
    let p = Paragraph::new(Line::from(Span::styled(hint, Style::new().fg(SUBTLE))));
    frame.render_widget(p, area);
}

// ── small helpers ────────────────────────────────────────────────────────────

fn header_line(s: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {s}"),
        Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
    ))
}
fn cmd_line(s: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(format!("  {}", s.into()), Style::new().fg(GREEN)))
}
fn plain(s: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(s.into(), Style::new().fg(SUBTLE)))
}

fn prebuilt_asset() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "splice-linux-x86_64",
        ("linux", "aarch64") => "splice-linux-aarch64",
        ("macos", "x86_64") => "splice-macos-x86_64",
        ("macos", "aarch64") => "splice-macos-aarch64",
        ("windows", _) => "splice-windows-x86_64.exe",
        _ => "build from source",
    }
}

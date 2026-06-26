//! `splice create <framework> [name]` — scaffold a front-end project wired to
//! `@codonsplice`.
//!
//! Wraps the official scaffolder (Vite for react/vue/svelte, `create-astro` for
//! astro), then injects the `@codonsplice/<framework>` dependency and a
//! ready-to-run SpliceQL demo, and finally runs `npm install`. The official
//! tooling does the heavy lifting so the generated project always matches the
//! framework's current conventions.

use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    DefaultTerminal, Frame,
};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Framework {
    React,
    Vue,
    Svelte,
    Astro,
}

/// The frameworks offered in the interactive menu, in display order.
const FRAMEWORKS: [Framework; 4] = [
    Framework::React,
    Framework::Vue,
    Framework::Svelte,
    Framework::Astro,
];

impl Framework {
    fn parse(s: &str) -> Option<Framework> {
        match s.to_ascii_lowercase().as_str() {
            "react" => Some(Framework::React),
            "vue" => Some(Framework::Vue),
            "svelte" => Some(Framework::Svelte),
            "astro" => Some(Framework::Astro),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Framework::React => "react",
            Framework::Vue => "vue",
            Framework::Svelte => "svelte",
            Framework::Astro => "astro",
        }
    }

    /// The `@codonsplice/*` wrapper package the demo's hook/composable comes from.
    fn pkg(self) -> &'static str {
        match self {
            Framework::React => "@codonsplice/react",
            Framework::Vue => "@codonsplice/vue",
            Framework::Svelte => "@codonsplice/svelte",
            Framework::Astro => "@codonsplice/astro",
        }
    }

    /// The `@codonsplice/*` packages the scaffolded demo depends on. Each wrapper
    /// re-exports the core tooling (`compile`/`check`/`execute`) from
    /// `@codonsplice/wasm`, so the app only needs the one wrapper package.
    fn deps(self) -> &'static [&'static str] {
        match self {
            Framework::React => &["@codonsplice/react"],
            Framework::Vue => &["@codonsplice/vue"],
            Framework::Svelte => &["@codonsplice/svelte"],
            Framework::Astro => &["@codonsplice/astro"],
        }
    }
}

/// `npm` (or `npm.cmd` on Windows).
fn npm() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

/// The official scaffolder invocation (program + args) for a fresh project.
/// Vite and create-astro are both told NOT to install, so we can inject the
/// `@codonsplice` dependency before a single `npm install`.
fn scaffold_command(fw: Framework, name: &str) -> (String, Vec<String>) {
    match fw {
        Framework::Astro => (
            npm().to_string(),
            [
                "create",
                "astro@latest",
                name,
                "--",
                "--template",
                "minimal",
                "--no-install",
                "--no-git",
                "--skip-houston",
                "--yes",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ),
        _ => {
            let template = match fw {
                Framework::React => "react",
                Framework::Vue => "vue",
                Framework::Svelte => "svelte",
                Framework::Astro => unreachable!(),
            };
            (
                npm().to_string(),
                ["create", "vite@latest", name, "--", "--template", template]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            )
        }
    }
}

/// Add the framework's `@codonsplice/*` packages to a `package.json`'s
/// `dependencies` (pinned to `latest`), returning the re-serialized JSON.
fn inject_dependencies(pkg_json: &str, fw: Framework) -> serde_json::Result<String> {
    let mut v: serde_json::Value = serde_json::from_str(pkg_json)?;
    let obj = v.as_object_mut().ok_or_else(|| {
        serde_json::from_str::<serde_json::Value>("\"package.json is not an object\"").unwrap_err()
    })?;
    let deps = obj
        .entry("dependencies")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(map) = deps.as_object_mut() {
        for pkg in fw.deps() {
            map.insert(
                pkg.to_string(),
                serde_json::Value::String("latest".to_string()),
            );
        }
    }
    let mut out = serde_json::to_string_pretty(&v)?;
    out.push('\n');
    Ok(out)
}

/// The demo file(s) to write: `(path relative to project root, contents)`.
/// All four import the shared cnvlens-styled stylesheet at `src/splice-demo.css`.
fn example_files(fw: Framework) -> Vec<(&'static str, &'static str)> {
    match fw {
        Framework::React => vec![("src/App.jsx", REACT_DEMO), ("src/splice-demo.css", CNV_CSS)],
        Framework::Vue => vec![("src/App.vue", VUE_DEMO), ("src/splice-demo.css", CNV_CSS)],
        Framework::Svelte => vec![("src/App.svelte", SVELTE_DEMO), ("src/splice-demo.css", CNV_CSS)],
        Framework::Astro => vec![("src/pages/index.astro", ASTRO_DEMO), ("src/splice-demo.css", CNV_CSS)],
    }
}

// ANSI colors for the "splicifying" UX.
const MAUVE: &str = "\x1b[38;5;183m";
const GREEN: &str = "\x1b[38;5;114m";
const RED: &str = "\x1b[38;5;210m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

type Spinner = (mpsc::Sender<()>, Option<thread::JoinHandle<()>>);

/// Start a colored spinner for `label` (animated on a TTY; one static line when
/// piped, so logs stay clean). Pair with [`stop_spinner`].
fn start_spinner(label: &str) -> Spinner {
    let (tx, rx) = mpsc::channel::<()>();
    if io::stdout().is_terminal() {
        let lbl = label.to_string();
        let h = thread::spawn(move || {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0;
            while rx.try_recv().is_err() {
                print!("\r{MAUVE}{}{RESET} {lbl}…", frames[i % frames.len()]);
                let _ = io::stdout().flush();
                i += 1;
                thread::sleep(Duration::from_millis(90));
            }
        });
        (tx, Some(h))
    } else {
        println!("{MAUVE}•{RESET} {label}…");
        (tx, None)
    }
}

fn stop_spinner((tx, handle): Spinner) {
    let _ = tx.send(());
    if let Some(h) = handle {
        let _ = h.join();
        print!("\r\x1b[2K"); // clear the spinner line
    }
}

/// Run a command with a spinner, hiding its output behind `label`.
///
/// stdin is `null` on purpose: it forces scaffolders like `create-vite` /
/// `create-astro` into their non-interactive path (they only prompt on a TTY),
/// so the project is created deterministically as `<name>` with our template
/// instead of dropping into an interactive wizard.
fn run_with_spinner(label: &str, prog: &str, args: &[String], cwd: Option<&Path>) -> bool {
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            println!("{RED}✗{RESET} {label}: {e}");
            return false;
        }
    };

    let spinner = start_spinner(label);
    let output = child.wait_with_output();
    stop_spinner(spinner);

    match output {
        Ok(o) if o.status.success() => {
            println!("{GREEN}✓{RESET} {label}");
            true
        }
        Ok(o) => {
            println!("{RED}✗{RESET} {label}");
            let err = String::from_utf8_lossy(&o.stderr);
            for line in err.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev() {
                println!("  {DIM}{line}{RESET}");
            }
            false
        }
        Err(e) => {
            println!("{RED}✗{RESET} {label}: {e}");
            false
        }
    }
}

/// Source for the bundled sample BAM (NA12878, EGFR region) — the same asset the
/// release pipeline verifies against.
const SAMPLE_BAM_URL: &str =
    "https://raw.githubusercontent.com/Pogo-Bash/cnvlens/master/public/sample-data/NA12878_EGFR.bam";

/// Download the sample BAM into `<root>/public/` so the preloaded query runs out
/// of the box (Vite/Astro serve `public/` at the site root).
fn download_sample_bam(root: &Path) {
    let label = "Fetching the sample BAM (NA12878, EGFR)";
    let spinner = start_spinner(label);
    let result = (|| -> Result<(), String> {
        let dir = root.join("public");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let resp = ureq::get(SAMPLE_BAM_URL)
            .timeout(Duration::from_secs(60))
            .call()
            .map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        resp.into_reader()
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        std::fs::write(dir.join("NA12878_EGFR.bam"), &buf).map_err(|e| e.to_string())?;
        Ok(())
    })();
    stop_spinner(spinner);
    match result {
        Ok(()) => println!("{GREEN}✓{RESET} {label}"),
        Err(e) => println!(
            "{RED}⚠{RESET} {label} failed ({e}) — add public/NA12878_EGFR.bam manually."
        ),
    }
}

pub fn cmd_create(framework: Option<String>, name: Option<String>) -> ExitCode {
    // With a framework arg, run directly; otherwise open the interactive menu.
    let (fw, name) = match framework {
        Some(s) => match Framework::parse(&s) {
            Some(f) => (f, name.unwrap_or_else(|| "splice-app".to_string())),
            None => {
                eprintln!("✗ unknown framework {s:?} — choose: react, vue, svelte, astro");
                return ExitCode::FAILURE;
            }
        },
        None => match run_menu() {
            Ok(Some(picked)) => picked,
            Ok(None) => {
                println!("cancelled — nothing created.");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("✗ menu error: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    scaffold(fw, &name)
}

/// Scaffold `<name>` for `fw`: official tooling → inject deps → demo → install,
/// with a colored "splicifying" flow.
fn scaffold(fw: Framework, name: &str) -> ExitCode {
    let root = Path::new(name);
    if root.exists() {
        eprintln!("{RED}✗{RESET} `{name}` already exists — choose another name or remove it.");
        return ExitCode::FAILURE;
    }

    println!("{MAUVE}{BOLD}✦ Splicifying your {} project «{name}»{RESET}\n", fw.label());

    // 1. Scaffold deterministically via the official tooling (stdin null).
    let (prog, args) = scaffold_command(fw, name);
    if !run_with_spinner(&format!("Scaffolding {} app", fw.label()), &prog, &args, None) {
        eprintln!("{RED}✗{RESET} scaffold failed (is Node.js / npm installed?).");
        return ExitCode::FAILURE;
    }
    if !root.join("package.json").exists() {
        eprintln!("{RED}✗{RESET} expected `{name}/` but the scaffolder didn't create it.");
        return ExitCode::FAILURE;
    }

    // 2. Wire in the @codonsplice dependency.
    let pkg_path = root.join("package.json");
    match std::fs::read_to_string(&pkg_path) {
        Ok(s) => match inject_dependencies(&s, fw) {
            Ok(updated) => {
                let _ = std::fs::write(&pkg_path, updated);
                println!("{GREEN}✓{RESET} Wired in {} (powered by @codonsplice/wasm)", fw.pkg());
            }
            Err(e) => eprintln!("{RED}⚠{RESET} could not edit package.json ({e})"),
        },
        Err(e) => eprintln!("{RED}⚠{RESET} could not read {} ({e})", pkg_path.display()),
    }

    // 3. Add our twist: replace the framework's default starter with the live
    //    SpliceQL demo (query → bytecode playground + Run button), and brand the
    //    page title.
    for (rel, contents) in example_files(fw) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, contents);
    }
    brand_index_html(root, fw);
    println!("{GREEN}✓{RESET} Added the SpliceQL demo (live playground + Run button)");

    // 4. Bundle the sample BAM so the preloaded query runs out of the box.
    download_sample_bam(root);

    // 5. Install dependencies (the wrapper + its wasm core).
    if !run_with_spinner("Installing dependencies", npm(), &["install".to_string()], Some(root)) {
        eprintln!("{RED}⚠{RESET} `npm install` failed — run it yourself in {name}/");
    }

    println!("\n{GREEN}{BOLD}✓ Splicified!{RESET} Your SpliceQL-powered app is ready.\n");
    println!("  cd {name}");
    println!("  npm run dev");
    ExitCode::SUCCESS
}

/// Brand the scaffolded `index.html` title (Vite frameworks). Best-effort.
fn brand_index_html(root: &Path, fw: Framework) {
    let idx = root.join("index.html");
    if let Ok(html) = std::fs::read_to_string(&idx) {
        // Replace whatever the template put in <title>…</title>.
        if let (Some(a), Some(b)) = (html.find("<title>"), html.find("</title>")) {
            if b > a {
                let mut out = String::with_capacity(html.len());
                out.push_str(&html[..a + "<title>".len()]);
                out.push_str(&format!("SpliceQL × {}", fw.label()));
                out.push_str(&html[b..]);
                let _ = std::fs::write(&idx, out);
            }
        }
    }
}

// ── Interactive menu ─────────────────────────────────────────────────────────

/// Mauve accent + subtle gray, matching the rest of the splice UI.
const ACCENT: Color = Color::Rgb(203, 166, 247);
const SUBTLE: Color = Color::Rgb(127, 132, 156);

#[derive(PartialEq, Clone, Copy)]
enum MenuFocus {
    Framework,
    Name,
}

struct Menu {
    fw_idx: usize,
    name: String,
    focus: MenuFocus,
    done: Option<Option<(Framework, String)>>, // Some(Some)=create, Some(None)=cancel
}

/// Run the `splice create` picker; returns the chosen `(framework, name)` or
/// `None` if cancelled. Collects the choice in a TUI, then hands back so the
/// scaffold runs on the normal terminal (npm needs inherited stdio).
fn run_menu() -> io::Result<Option<(Framework, String)>> {
    let mut terminal = ratatui::init();
    let result = menu_loop(&mut terminal);
    ratatui::restore();
    result
}

fn menu_loop(terminal: &mut DefaultTerminal) -> io::Result<Option<(Framework, String)>> {
    let mut m = Menu {
        fw_idx: 0,
        name: "splice-app".to_string(),
        focus: MenuFocus::Framework,
        done: None,
    };
    loop {
        terminal.draw(|f| draw_menu(f, &m))?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Esc => m.done = Some(None),
                KeyCode::Char('c') if ctrl => m.done = Some(None),
                // Up/Down (and Tab) move between the Framework and Name fields.
                KeyCode::Up | KeyCode::Down | KeyCode::Tab => {
                    m.focus = match m.focus {
                        MenuFocus::Framework => MenuFocus::Name,
                        MenuFocus::Name => MenuFocus::Framework,
                    };
                }
                // Left/Right cycle the framework when it's focused.
                KeyCode::Left if m.focus == MenuFocus::Framework => {
                    m.fw_idx = m.fw_idx.saturating_sub(1);
                }
                KeyCode::Right if m.focus == MenuFocus::Framework => {
                    m.fw_idx = (m.fw_idx + 1).min(FRAMEWORKS.len() - 1);
                }
                KeyCode::Enter => {
                    let name = if m.name.trim().is_empty() {
                        "splice-app".to_string()
                    } else {
                        m.name.trim().to_string()
                    };
                    m.done = Some(Some((FRAMEWORKS[m.fw_idx], name)));
                }
                // Editing the name (only when that field is focused).
                KeyCode::Backspace if m.focus == MenuFocus::Name => {
                    m.name.pop();
                }
                KeyCode::Char(c)
                    if m.focus == MenuFocus::Name
                        && (c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) =>
                {
                    m.name.push(c)
                }
                _ => {}
            }
        }
        if let Some(choice) = m.done {
            return Ok(choice);
        }
    }
}

fn draw_menu(frame: &mut Frame, m: &Menu) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // title
            Constraint::Length(3), // framework row
            Constraint::Length(3), // name
            Constraint::Min(3),    // wasm note + hints
        ])
        .split(frame.area());

    let title = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("🧬 splice create", Style::new().fg(ACCENT).bold()),
            Span::styled("  — new SpliceQL project", Style::new().fg(SUBTLE)),
        ]),
        Line::from(Span::styled(
            "Scaffold a web app with the genomic query engine wired in.",
            Style::new().fg(SUBTLE),
        )),
    ])
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, rows[0]);

    let fw_focused = m.focus == MenuFocus::Framework;
    // Framework choices on one row; ◂ ▸ hint when focused.
    let mut spans = vec![Span::styled(
        "Framework  ",
        Style::new().fg(SUBTLE).bold(),
    )];
    for (i, fw) in FRAMEWORKS.iter().enumerate() {
        let selected = i == m.fw_idx;
        let style = if selected && fw_focused {
            Style::new().fg(Color::Black).bg(ACCENT).bold()
        } else if selected {
            Style::new().fg(ACCENT).bold()
        } else {
            Style::new().fg(Color::Gray)
        };
        spans.push(Span::styled(format!(" {} ", fw.label()), style));
        spans.push(Span::raw(" "));
    }
    if fw_focused {
        spans.push(Span::styled("  ◂ ▸", Style::new().fg(SUBTLE)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(field_block(fw_focused)),
        rows[1],
    );

    let name_focused = m.focus == MenuFocus::Name;
    let mut name_spans = vec![
        Span::styled("Name  ", Style::new().fg(SUBTLE).bold()),
        Span::styled(&m.name, Style::new().fg(Color::Cyan)),
    ];
    if name_focused {
        name_spans.push(Span::styled("▏", Style::new().fg(ACCENT)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(name_spans)).block(field_block(name_focused)),
        rows[2],
    );

    let info = Paragraph::new(vec![
        Line::from(Span::styled(
            "Pre-wired with @codonsplice/wasm — a live SpliceQL playground that",
            Style::new().fg(Color::Green),
        )),
        Line::from(Span::styled(
            "type-checks + compiles your query to bytecode in the browser.",
            Style::new().fg(Color::Green),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "↑/↓ move between fields · ◂ ▸ pick framework · type to name · Enter create · Esc cancel",
            Style::new().fg(Color::DarkGray),
        )),
    ])
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(info, rows[3]);
}

/// A bordered block whose border brightens when the field is focused.
fn field_block(focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(ACCENT)
    } else {
        Style::new().fg(SUBTLE)
    };
    Block::default().borders(Borders::ALL).border_style(style)
}

// ── Demo templates ───────────────────────────────────────────────────────────

/// Shared stylesheet — mirrors the cnvlens look (Catppuccin Mocha, JetBrains
/// Mono, thin scrollbars, card/table styling). Imported by every demo.
const CNV_CSS: &str = r#"@import url('https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@300;400;500;700&display=swap');

:root {
  --crust: #11111b; --base: #1e1e2e; --mantle: #181825;
  --surface0: #313244; --surface1: #45475a; --overlay1: #7f849c;
  --text: #cdd6f4; --subtext0: #a6adc8; --subtext1: #bac2de;
  --mauve: #cba6f7; --lavender: #b4befe; --green: #a6e3a1; --red: #f38ba8;
  --blue: #89b4fa; --peach: #fab387;
  color-scheme: dark;
}
* { box-sizing: border-box; }
html, body { margin: 0; height: 100%; }
body {
  background: var(--crust);
  color: var(--text);
  font-family: 'JetBrains Mono', ui-monospace, monospace;
  -webkit-font-smoothing: antialiased;
}
::selection { background: rgba(203, 166, 247, .2); color: var(--mauve); }
::-webkit-scrollbar { width: 6px; height: 6px; }
::-webkit-scrollbar-track { background: var(--crust); }
::-webkit-scrollbar-thumb { background: var(--surface0); border-radius: 3px; }
::-webkit-scrollbar-thumb:hover { background: var(--mauve); }
* { scrollbar-width: thin; scrollbar-color: var(--surface0) var(--crust); }
:focus-visible { outline: 2px solid var(--mauve); outline-offset: 2px; }

.wrap { max-width: 960px; margin: 0 auto; padding: 2.5rem 1.25rem 4rem; }
.head { display: flex; align-items: baseline; gap: .6rem; }
.head .logo { color: var(--mauve); font-weight: 700; font-size: 1.3rem; letter-spacing: -.02em; }
.head .tag { color: var(--overlay1); font-size: .8rem; }
.sub { color: var(--subtext0); font-size: .85rem; line-height: 1.55; margin: .4rem 0 1.5rem; max-width: 64ch; }

.card { border: 1px solid var(--surface0); border-radius: .5rem; background: var(--base); padding: 1.25rem; }
.label { display: block; font-size: .72rem; font-weight: 700; text-transform: uppercase; letter-spacing: .08em; color: var(--subtext1); margin-bottom: .6rem; }

.editor {
  width: 100%; min-height: 150px; resize: vertical;
  background: var(--crust); color: var(--text);
  border: 1px solid var(--surface0); border-radius: .5rem; padding: .85rem 1rem;
  font-family: inherit; font-size: .85rem; line-height: 1.6;
}
.editor:focus { outline: none; border-color: var(--mauve); box-shadow: 0 0 0 1px rgba(203, 166, 247, .3); }

.controls { display: flex; align-items: center; gap: 1rem; margin-top: 1rem; flex-wrap: wrap; }
.status { display: inline-flex; align-items: center; gap: .5rem; font-size: .8rem; padding: .4rem .8rem; border: 1px solid var(--surface0); border-radius: .5rem; }
.status .dot { width: .5rem; height: .5rem; border-radius: 50%; flex-shrink: 0; }
.status.ok { color: var(--green); } .status.ok .dot { background: var(--green); }
.status.bad { color: var(--red); } .status.bad .dot { background: var(--red); }

.btn {
  margin-left: auto; display: inline-flex; align-items: center; gap: .5rem;
  font-family: inherit; font-weight: 700; font-size: .85rem;
  padding: .5rem 1.1rem; border-radius: .5rem; cursor: pointer;
  border: 1px solid var(--mauve); background: var(--mauve); color: var(--crust);
  transition: all .2s;
}
.btn:hover:not(:disabled) { background: var(--lavender); border-color: var(--lavender); }
.btn:disabled { opacity: .5; cursor: not-allowed; }

.grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; margin-top: 1rem; }
@media (max-width: 760px) { .grid { grid-template-columns: 1fr; } }

.term { margin: 0; background: var(--crust); border: 1px solid var(--surface0); border-radius: .5rem; padding: 1rem; font-size: .78rem; line-height: 1.55; color: var(--subtext1); white-space: pre; overflow: auto; max-height: 300px; }

.results { border: 1px solid var(--surface0); border-radius: .5rem; overflow: auto; max-height: 300px; }
table { width: 100%; border-collapse: collapse; font-size: .8rem; }
thead th { position: sticky; top: 0; text-align: left; font-size: .68rem; font-weight: 700; text-transform: uppercase; letter-spacing: .06em; color: var(--subtext1); padding: .55rem .75rem; border-bottom: 1px solid var(--surface0); background: var(--base); }
tbody td { padding: .5rem .75rem; border-bottom: 1px solid rgba(49, 50, 68, .5); color: var(--subtext1); }
tbody tr:nth-child(even) { background: rgba(49, 50, 68, .08); }
tbody tr:hover { background: rgba(49, 50, 68, .3); }
td.num { text-align: right; font-variant-numeric: tabular-nums; }
.empty { color: var(--overlay1); font-size: .8rem; padding: 1rem; }
.err { color: var(--red); white-space: pre-wrap; font-size: .8rem; margin: .75rem 0 0; }
"#;

const REACT_DEMO: &str = r#"import { useState, useEffect, useCallback } from 'react'
import { useSpliceQL, compile, check } from '@codonsplice/react'
import './splice-demo.css'

const QUERY = `FROM bam "NA12878_EGFR.bam"
WHERE chr = "7"
  AND depth > 20
CALL variants
WITH min_af = 0.05,
     min_depth = 10
LIMIT 5`

export default function App() {
  const { execute, result, error, loading } = useSpliceQL()
  const [query, setQuery] = useState(QUERY)
  const [bytecode, setBytecode] = useState('')
  const [typeError, setTypeError] = useState(null)
  const [files, setFiles] = useState(null)

  // Load the bundled sample BAM once, then run the preloaded query.
  useEffect(() => {
    let on = true
    fetch('/NA12878_EGFR.bam')
      .then((r) => r.arrayBuffer())
      .then((buf) => {
        if (!on) return
        const f = { 'NA12878_EGFR.bam': new Uint8Array(buf) }
        setFiles(f)
        execute({ query: QUERY, files: f }).catch(() => {})
      })
      .catch(() => {})
    return () => { on = false }
  }, [execute])

  // Live: type-check + compile to bytecode as you type.
  useEffect(() => {
    let on = true
    ;(async () => {
      try {
        const err = await check(query)
        if (!on) return
        setTypeError(err)
        setBytecode(err ? '' : await compile(query))
      } catch (e) { if (on) { setTypeError(String(e)); setBytecode('') } }
    })()
    return () => { on = false }
  }, [query])

  const run = useCallback(() => { if (files) execute({ query, files }) }, [execute, files, query])

  const rows = Array.isArray(result) ? result : []
  const cols = rows.length ? Object.keys(rows[0]) : []

  return (
    <div className="wrap">
      <div className="head">
        <span className="logo">SpliceQL</span>
        <span className="tag">× React · @codonsplice/wasm</span>
      </div>
      <p className="sub">
        Genomic queries compiled to WebAssembly and run in your browser — no server.
        The sample BAM (NA12878, EGFR region) is bundled; edit the query and run it.
      </p>

      <div className="card">
        <label className="label">Query</label>
        <textarea className="editor" spellCheck={false} value={query} onChange={(e) => setQuery(e.target.value)} />
        <div className="controls">
          {typeError
            ? <span className="status bad"><span className="dot" />invalid</span>
            : <span className="status ok"><span className="dot" />valid SpliceQL</span>}
          <button className="btn" onClick={run} disabled={loading || !files || !!typeError}>
            {loading ? 'Running…' : files ? 'Run query' : 'Loading BAM…'}
          </button>
        </div>
        {error && <pre className="err">{String(error)}</pre>}
      </div>

      <div className="grid">
        <div className="card">
          <label className="label">Compiled bytecode</label>
          <pre className="term">{bytecode || '—'}</pre>
        </div>
        <div className="card">
          <label className="label">Results{rows.length ? ` · ${rows.length}` : ''}</label>
          {rows.length ? (
            <div className="results">
              <table>
                <thead><tr>{cols.map((c) => <th key={c}>{c}</th>)}</tr></thead>
                <tbody>
                  {rows.map((r, i) => (
                    <tr key={i}>{cols.map((c) => (
                      <td key={c} className={typeof r[c] === 'number' ? 'num' : ''}>{String(r[c])}</td>
                    ))}</tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : <div className="empty">No results yet — run the query.</div>}
        </div>
      </div>
    </div>
  )
}
"#;

const VUE_DEMO: &str = r#"<script setup>
import { ref, watch, onMounted, computed } from 'vue'
import { useSpliceQL, compile, check } from '@codonsplice/vue'
import './splice-demo.css'

const QUERY = `FROM bam "NA12878_EGFR.bam"
WHERE chr = "7"
  AND depth > 20
CALL variants
WITH min_af = 0.05,
     min_depth = 10
LIMIT 5`

const { execute, result, error, loading } = useSpliceQL()
const query = ref(QUERY)
const bytecode = ref('')
const typeError = ref(null)
const files = ref(null)

const rows = computed(() => (Array.isArray(result.value) ? result.value : []))
const cols = computed(() => (rows.value.length ? Object.keys(rows.value[0]) : []))

onMounted(async () => {
  try {
    const buf = await (await fetch('/NA12878_EGFR.bam')).arrayBuffer()
    files.value = { 'NA12878_EGFR.bam': new Uint8Array(buf) }
    await execute({ query: QUERY, files: files.value })
  } catch (e) { /* leave the demo idle if the BAM is missing */ }
})

watch(query, async (q) => {
  try {
    const err = await check(q)
    typeError.value = err
    bytecode.value = err ? '' : await compile(q)
  } catch (e) { typeError.value = String(e); bytecode.value = '' }
}, { immediate: true })

function run() { if (files.value) execute({ query: query.value, files: files.value }) }
</script>

<template>
  <div class="wrap">
    <div class="head">
      <span class="logo">SpliceQL</span>
      <span class="tag">× Vue · @codonsplice/wasm</span>
    </div>
    <p class="sub">
      Genomic queries compiled to WebAssembly and run in your browser — no server.
      The sample BAM (NA12878, EGFR region) is bundled; edit the query and run it.
    </p>

    <div class="card">
      <label class="label">Query</label>
      <textarea class="editor" spellcheck="false" v-model="query"></textarea>
      <div class="controls">
        <span v-if="typeError" class="status bad"><span class="dot"></span>invalid</span>
        <span v-else class="status ok"><span class="dot"></span>valid SpliceQL</span>
        <button class="btn" @click="run" :disabled="loading || !files || !!typeError">
          {{ loading ? 'Running…' : files ? 'Run query' : 'Loading BAM…' }}
        </button>
      </div>
      <pre v-if="error" class="err">{{ String(error) }}</pre>
    </div>

    <div class="grid">
      <div class="card">
        <label class="label">Compiled bytecode</label>
        <pre class="term">{{ bytecode || '—' }}</pre>
      </div>
      <div class="card">
        <label class="label">Results{{ rows.length ? ` · ${rows.length}` : '' }}</label>
        <div v-if="rows.length" class="results">
          <table>
            <thead><tr><th v-for="c in cols" :key="c">{{ c }}</th></tr></thead>
            <tbody>
              <tr v-for="(r, i) in rows" :key="i">
                <td v-for="c in cols" :key="c" :class="typeof r[c] === 'number' ? 'num' : ''">{{ r[c] }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-else class="empty">No results yet — run the query.</div>
      </div>
    </div>
  </div>
</template>
"#;

const SVELTE_DEMO: &str = r#"<script>
  import { onMount } from 'svelte'
  import { createSpliceQL, compile, check } from '@codonsplice/svelte'
  import './splice-demo.css'

  const QUERY = `FROM bam "NA12878_EGFR.bam"
WHERE chr = "7"
  AND depth > 20
CALL variants
WITH min_af = 0.05,
     min_depth = 10
LIMIT 5`

  const { execute, result, error, loading } = createSpliceQL()
  let query = QUERY
  let bytecode = ''
  let typeError = null
  let files = null

  onMount(async () => {
    try {
      const buf = await (await fetch('/NA12878_EGFR.bam')).arrayBuffer()
      files = { 'NA12878_EGFR.bam': new Uint8Array(buf) }
      await execute({ query: QUERY, files })
    } catch (e) { /* leave idle if the BAM is missing */ }
  })

  $: liveCompile(query)
  async function liveCompile(q) {
    try {
      const err = await check(q)
      typeError = err
      bytecode = err ? '' : await compile(q)
    } catch (e) { typeError = String(e); bytecode = '' }
  }
  function run() { if (files) execute({ query, files }) }

  $: rows = Array.isArray($result) ? $result : []
  $: cols = rows.length ? Object.keys(rows[0]) : []
</script>

<div class="wrap">
  <div class="head">
    <span class="logo">SpliceQL</span>
    <span class="tag">× Svelte · @codonsplice/wasm</span>
  </div>
  <p class="sub">
    Genomic queries compiled to WebAssembly and run in your browser — no server.
    The sample BAM (NA12878, EGFR region) is bundled; edit the query and run it.
  </p>

  <div class="card">
    <label class="label">Query</label>
    <textarea class="editor" spellcheck="false" bind:value={query}></textarea>
    <div class="controls">
      {#if typeError}<span class="status bad"><span class="dot"></span>invalid</span>
      {:else}<span class="status ok"><span class="dot"></span>valid SpliceQL</span>{/if}
      <button class="btn" on:click={run} disabled={$loading || !files || !!typeError}>
        {$loading ? 'Running…' : files ? 'Run query' : 'Loading BAM…'}
      </button>
    </div>
    {#if $error}<pre class="err">{String($error)}</pre>{/if}
  </div>

  <div class="grid">
    <div class="card">
      <label class="label">Compiled bytecode</label>
      <pre class="term">{bytecode || '—'}</pre>
    </div>
    <div class="card">
      <label class="label">Results{rows.length ? ` · ${rows.length}` : ''}</label>
      {#if rows.length}
        <div class="results">
          <table>
            <thead><tr>{#each cols as c}<th>{c}</th>{/each}</tr></thead>
            <tbody>
              {#each rows as r}
                <tr>{#each cols as c}<td class={typeof r[c] === 'number' ? 'num' : ''}>{r[c]}</td>{/each}</tr>
              {/each}
            </tbody>
          </table>
        </div>
      {:else}<div class="empty">No results yet — run the query.</div>{/if}
    </div>
  </div>
</div>
"#;

const ASTRO_DEMO: &str = r#"---
import '../splice-demo.css'
const query = `FROM bam "NA12878_EGFR.bam"
WHERE chr = "7"
  AND depth > 20
CALL variants
WITH min_af = 0.05,
     min_depth = 10
LIMIT 5`
---

<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>SpliceQL × Astro</title>
  </head>
  <body>
    <div class="wrap">
      <div class="head">
        <span class="logo">SpliceQL</span>
        <span class="tag">× Astro · @codonsplice/wasm</span>
      </div>
      <p class="sub">
        Genomic queries compiled to WebAssembly and run in your browser — no server.
        The sample BAM (NA12878, EGFR region) is bundled; edit the query and run it.
      </p>

      <div class="card">
        <label class="label">Query</label>
        <textarea id="q" class="editor" spellcheck="false">{query}</textarea>
        <div class="controls">
          <span id="status" class="status ok"><span class="dot"></span>valid SpliceQL</span>
          <button id="run" class="btn" disabled>Loading BAM…</button>
        </div>
        <pre id="err" class="err" style="display:none"></pre>
      </div>

      <div class="grid">
        <div class="card">
          <label class="label">Compiled bytecode</label>
          <pre id="bytecode" class="term">—</pre>
        </div>
        <div class="card">
          <label class="label" id="resLabel">Results</label>
          <div id="results"><div class="empty">No results yet — run the query.</div></div>
        </div>
      </div>
    </div>

    <script>
      import { execute, compile, check } from '@codonsplice/astro'
      const $ = (id) => document.getElementById(id)
      const q = $('q'), statusEl = $('status'), bytecode = $('bytecode')
      const errEl = $('err'), runBtn = $('run'), results = $('results'), resLabel = $('resLabel')
      let files = null

      function setStatus(err) {
        statusEl.className = 'status ' + (err ? 'bad' : 'ok')
        statusEl.innerHTML = '<span class="dot"></span>' + (err ? 'invalid' : 'valid SpliceQL')
      }
      async function live() {
        try {
          const err = await check(q.value)
          setStatus(err)
          bytecode.textContent = err ? '—' : await compile(q.value)
        } catch (e) { setStatus(true); bytecode.textContent = '—' }
      }
      function render(rows) {
        if (!Array.isArray(rows) || !rows.length) {
          results.innerHTML = '<div class="empty">No results.</div>'; resLabel.textContent = 'Results'; return
        }
        const cols = Object.keys(rows[0])
        resLabel.textContent = 'Results · ' + rows.length
        const th = cols.map((c) => `<th>${c}</th>`).join('')
        const tb = rows.map((r) => '<tr>' + cols.map((c) =>
          `<td class="${typeof r[c] === 'number' ? 'num' : ''}">${r[c]}</td>`).join('') + '</tr>').join('')
        results.innerHTML = `<div class="results"><table><thead><tr>${th}</tr></thead><tbody>${tb}</tbody></table></div>`
      }
      async function run() {
        if (!files) return
        runBtn.disabled = true; runBtn.textContent = 'Running…'; errEl.style.display = 'none'
        try { render(await execute({ query: q.value, files })) }
        catch (e) { errEl.style.display = 'block'; errEl.textContent = String(e) }
        finally { runBtn.disabled = false; runBtn.textContent = 'Run query' }
      }
      q.addEventListener('input', live); live()
      runBtn.addEventListener('click', run)
      ;(async () => {
        try {
          const buf = await (await fetch('/NA12878_EGFR.bam')).arrayBuffer()
          files = { 'NA12878_EGFR.bam': new Uint8Array(buf) }
          runBtn.disabled = false; runBtn.textContent = 'Run query'; run()
        } catch (e) { runBtn.textContent = 'BAM not found' }
      })()
    </script>
  </body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frameworks_case_insensitively() {
        assert_eq!(Framework::parse("React"), Some(Framework::React));
        assert_eq!(Framework::parse("vue"), Some(Framework::Vue));
        assert_eq!(Framework::parse("SVELTE"), Some(Framework::Svelte));
        assert_eq!(Framework::parse("astro"), Some(Framework::Astro));
        assert_eq!(Framework::parse("angular"), None);
    }

    #[test]
    fn vite_scaffold_is_noninstall_with_template() {
        let (prog, args) = scaffold_command(Framework::React, "demo");
        assert!(prog.starts_with("npm"));
        assert_eq!(
            args,
            vec!["create", "vite@latest", "demo", "--", "--template", "react"]
        );
    }

    #[test]
    fn astro_scaffold_skips_install_and_git() {
        let (_, args) = scaffold_command(Framework::Astro, "demo");
        assert!(args.contains(&"create".to_string()));
        assert!(args.contains(&"astro@latest".to_string()));
        assert!(args.contains(&"--no-install".to_string()));
        assert!(args.contains(&"--no-git".to_string()));
    }

    #[test]
    fn injects_dependencies_into_existing_deps() {
        let pkg = r#"{"name":"demo","dependencies":{"react":"^18.0.0"}}"#;
        let out = inject_dependencies(pkg, Framework::React).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["dependencies"]["react"], "^18.0.0");
        assert_eq!(v["dependencies"]["@codonsplice/react"], "latest");
        // The wrapper re-exports the core tooling, so wasm is transitive only.
        assert!(v["dependencies"]["@codonsplice/wasm"].is_null());
    }

    #[test]
    fn injects_dependencies_when_deps_absent() {
        let pkg = r#"{"name":"demo"}"#;
        let out = inject_dependencies(pkg, Framework::Astro).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["dependencies"]["@codonsplice/astro"], "latest");
        // Astro re-exports the core API, so it doesn't add @codonsplice/wasm.
        assert!(v["dependencies"]["@codonsplice/wasm"].is_null());
    }

    #[test]
    fn example_paths_match_framework() {
        assert_eq!(example_files(Framework::React)[0].0, "src/App.jsx");
        assert_eq!(example_files(Framework::Vue)[0].0, "src/App.vue");
        assert_eq!(example_files(Framework::Svelte)[0].0, "src/App.svelte");
        assert_eq!(example_files(Framework::Astro)[0].0, "src/pages/index.astro");
        // React ships a separate stylesheet too.
        assert!(example_files(Framework::React)
            .iter()
            .any(|(p, _)| *p == "src/splice-demo.css"));
        // Each demo wires up the SpliceQL tooling.
        assert!(example_files(Framework::React)[0].1.contains("@codonsplice/react"));
        assert!(example_files(Framework::React)[0].1.contains("compile"));
        assert!(example_files(Framework::Astro)[0].1.contains("@codonsplice/astro"));
    }
}

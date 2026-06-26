//! The interactive CodonSplice TUI.
//!
//! Three panes are always visible: an EDITOR (top-left), an OUTPUT pane
//! (top-right), and a fixed ARCHITECTURE panel (bottom) that makes the
//! two-crate design explicit at all times.

use std::io::{self, BufRead, BufReader};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use codonsplice_core::{compile, disassemble, Vm, VmOutput};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    DefaultTerminal, Frame,
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

const DEFAULT_QUERY: &str = "FROM bam \"sample.bam\"\nWHERE depth > 30\nCALL variants\nWITH min_allele_freq = 0.05";

#[derive(PartialEq, Clone, Copy)]
enum Focus {
    Editor,
    Output,
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum Tab {
    Editor,
    Build,
}

/// Cross-compile / build targets offered in the BUILD tab.
const BUILD_TARGETS: &[(&str, Option<&str>, bool)] = &[
    ("native (host)", None, false),
    ("linux-x86_64", Some("x86_64-unknown-linux-gnu"), false),
    ("linux-aarch64", Some("aarch64-unknown-linux-gnu"), false),
    ("macos-x86_64", Some("x86_64-apple-darwin"), false),
    ("macos-aarch64", Some("aarch64-apple-darwin"), false),
    ("windows-x86_64", Some("x86_64-pc-windows-msvc"), false),
    ("wasm", None, true),
];

/// What the OUTPUT pane is currently showing.
struct OutputPane {
    title: String,
    lines: Vec<Line<'static>>,
}

impl OutputPane {
    fn welcome() -> Self {
        OutputPane {
            title: "OUTPUT".into(),
            lines: vec![
                Line::from("Press Ctrl+Enter (or F5) to compile + run."),
                Line::from("Ctrl+D: bytecode   Ctrl+A: AST   F1: help   Ctrl+Q: quit"),
                Line::from("New: run `splice install` for the guided installer (INSTALL flow)."),
            ],
        }
    }

    fn from_text(title: &str, text: &str) -> Self {
        OutputPane {
            title: title.into(),
            lines: text.lines().map(|l| Line::from(l.to_string())).collect(),
        }
    }
}

struct App {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    focus: Focus,
    output: OutputPane,
    scroll: u16,
    show_help: bool,
    quit: bool,
    tab: Tab,
    build_target: usize,
    build_release: bool,
    /// rustup targets installed on this machine (for marking BUILD targets that
    /// would fail to cross-compile). Empty if rustup isn't available.
    installed_targets: Vec<String>,
    /// Async build state: receiver for streamed output, accumulated lines, and
    /// a spinner frame. `building` is true while a `splice build` runs off-thread
    /// so the TUI stays responsive (the build used to block the event loop).
    build_rx: Option<mpsc::Receiver<BuildMsg>>,
    build_lines: Vec<String>,
    build_label: String,
    building: bool,
    spinner: usize,
}

/// A line of streamed build output, or a terminal "done" with the exit status.
enum BuildMsg {
    Line(String),
    Done(bool),
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl App {
    fn new() -> Self {
        let lines: Vec<String> = DEFAULT_QUERY.lines().map(String::from).collect();
        let cursor_row = lines.len().saturating_sub(1);
        let cursor_col = lines.last().map(|l| l.chars().count()).unwrap_or(0);
        App {
            lines,
            cursor_row,
            cursor_col,
            focus: Focus::Editor,
            output: OutputPane::welcome(),
            scroll: 0,
            show_help: false,
            quit: false,
            tab: Tab::Editor,
            build_target: 0,
            build_release: true,
            installed_targets: detect_installed_targets(),
            build_rx: None,
            build_lines: Vec::new(),
            build_label: String::new(),
            building: false,
            spinner: 0,
        }
    }

    fn source(&self) -> String {
        self.lines.join("\n")
    }

    // ── Actions ──────────────────────────────────────────────────────────────

    fn run_query(&mut self) {
        let src = self.source();
        match compile(&src) {
            Ok(program) => {
                let bytes = program.code.len();
                let msg = match Vm::new(program).run() {
                    Ok(VmOutput::Ready(_)) => {
                        format!("✓ compiled and reached HALT ({bytes} bytes of bytecode).")
                    }
                    Ok(VmOutput::Text(t)) => t,
                    Ok(VmOutput::Records(records)) | Ok(VmOutput::Rows(records)) => {
                        let mut s = String::new();
                        for r in records.iter().take(100) {
                            s.push_str(&codonsplice_core::vm::record_to_json(r).to_string());
                            s.push('\n');
                        }
                        if records.len() > 100 {
                            s.push_str(&format!("… {} more\n", records.len() - 100));
                        }
                        s.push_str(&format!("\n({} record(s))", records.len()));
                        s
                    }
                    Err(e) => format!("runtime error: {e}"),
                };
                self.output = OutputPane::from_text("OUTPUT · run", &msg);
            }
            Err(e) => self.show_error(&src, &e),
        }
        self.scroll = 0;
    }

    /// The rustup target triple a BUILD_TARGETS entry needs, if any (native host
    /// needs none). wasm needs `wasm32-unknown-unknown`.
    fn required_triple(idx: usize) -> Option<&'static str> {
        let (_, target, wasm) = BUILD_TARGETS[idx];
        if wasm {
            Some("wasm32-unknown-unknown")
        } else {
            target
        }
    }

    /// Whether the target at `idx` can be built here (its rustup target is
    /// installed). The native host target is always available.
    fn target_available(&self, idx: usize) -> bool {
        match Self::required_triple(idx) {
            None => true,
            Some(t) => self.installed_targets.iter().any(|s| s == t),
        }
    }

    /// Kick off a `splice build` subprocess off-thread, streaming its output
    /// into the OUTPUT pane so the TUI stays responsive (a spinner animates while
    /// it runs). No-op if a build is already in flight. Uninstalled cross-targets
    /// show an install hint instead of a long, failing build.
    fn run_build(&mut self) {
        if self.building {
            return; // a build is already running
        }
        let (label, target, wasm) = BUILD_TARGETS[self.build_target];

        // Guard: don't spawn a cross-compile that's doomed for a missing target.
        if !self.target_available(self.build_target) {
            let triple = Self::required_triple(self.build_target).unwrap_or("");
            self.output = OutputPane::from_text(
                "OUTPUT · build",
                &format!(
                    "target `{label}` is not installed.\n\nAdd it with:\n  rustup target add {triple}\n\nThen rebuild (Ctrl+Enter)."
                ),
            );
            return;
        }
        let name = self.build_output_name();

        // Write the editor content to a temp .spq.
        let tmp = std::env::temp_dir().join(format!("splice-tui-{}.spq", std::process::id()));
        if std::fs::write(&tmp, self.source()).is_err() {
            self.output = OutputPane::from_text("OUTPUT · build", "error: could not write temp file");
            return;
        }
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                self.output = OutputPane::from_text("OUTPUT · build", &format!("error: {e}"));
                return;
            }
        };

        let mut args: Vec<String> = vec![
            "build".into(),
            tmp.to_string_lossy().into_owned(),
            "-o".into(),
            name,
            "--no-update".into(),
        ];
        if self.build_release {
            args.push("--release".into());
        }
        if wasm {
            args.push("--wasm".into());
        } else if let Some(t) = target {
            args.push("--target".into());
            args.push(t.to_string());
        }

        self.building = true;
        self.build_label = label.to_string();
        self.build_lines = vec![format!("$ splice build (target: {label})"), String::new()];
        self.scroll = 0;
        self.refresh_build_output();

        let (tx, rx) = mpsc::channel::<BuildMsg>();
        self.build_rx = Some(rx);
        thread::spawn(move || {
            let ok = run_streamed_build(&exe, &args, &tx);
            let _ = std::fs::remove_file(&tmp);
            let _ = tx.send(BuildMsg::Done(ok));
        });
    }

    /// Drain streamed build output and advance the spinner. Called every tick.
    fn tick(&mut self) {
        if !self.building {
            return;
        }
        self.spinner = (self.spinner + 1) % SPINNER.len();
        let mut finished = None;
        if let Some(rx) = &self.build_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    BuildMsg::Line(l) => self.build_lines.push(l),
                    BuildMsg::Done(ok) => finished = Some(ok),
                }
            }
        }
        if let Some(ok) = finished {
            self.building = false;
            self.build_rx = None;
            self.build_lines
                .push(if ok { "✓ build complete".into() } else { "✗ build failed".into() });
        }
        self.refresh_build_output();
    }

    /// Rebuild the OUTPUT pane from the accumulated build lines, prefixing a
    /// spinner status line while the build is running.
    fn refresh_build_output(&mut self) {
        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.building {
            lines.push(Line::from(Span::styled(
                format!("{} building ({})…", SPINNER[self.spinner], self.build_label),
                Style::new().fg(Color::Rgb(250, 179, 135)),
            )));
        }
        for l in &self.build_lines {
            let style = if l.contains('✗') || l.to_lowercase().contains("error") {
                Style::new().fg(Color::Rgb(243, 139, 168))
            } else if l.starts_with('✓') {
                Style::new().fg(Color::Rgb(166, 227, 161))
            } else {
                Style::new().fg(Color::Gray)
            };
            lines.push(Line::from(Span::styled(l.clone(), style)));
        }
        self.output = OutputPane {
            title: "OUTPUT · build".into(),
            lines,
        };
    }

    /// The output binary name: @name directive, else "query".
    fn build_output_name(&self) -> String {
        let src = self.source();
        let (dirs, _) = crate::directive::parse_directives(&src);
        dirs.name.unwrap_or_else(|| "query".to_string())
    }

    /// `$variables` referenced in the editor content (with @input metadata).
    fn detected_vars(&self) -> Vec<(String, String)> {
        let src = self.source();
        let (dirs, _) = crate::directive::parse_directives(&src);
        crate::directive::scan_vars(&src)
            .into_iter()
            .map(|name| {
                let meta = match dirs.input(&name) {
                    Some(i) => {
                        let req = if i.required { "required" } else { "optional" };
                        match &i.default {
                            Some(d) => format!("{req}, default {d}"),
                            None => req.to_string(),
                        }
                    }
                    None => "undeclared".to_string(),
                };
                (name, meta)
            })
            .collect()
    }

    fn show_disassembly(&mut self) {
        let src = self.source();
        match compile(&src) {
            Ok(program) => {
                let asm = disassemble(&program);
                let lines = asm.lines().map(highlight_disasm).collect();
                self.output = OutputPane {
                    title: "OUTPUT · bytecode".into(),
                    lines,
                };
            }
            Err(e) => self.show_error(&src, &e),
        }
        self.scroll = 0;
    }

    fn show_ast(&mut self) {
        let src = self.source();
        match spliceql::parse(&src) {
            Ok(q) => self.output = OutputPane::from_text("OUTPUT · AST", &crate::pretty_ast(&q)),
            Err(e) => {
                let ce = codonsplice_core::CompileError::ParseError(e);
                self.show_error(&src, &ce);
            }
        }
        self.scroll = 0;
    }

    fn show_error(&mut self, src: &str, err: &codonsplice_core::CompileError) {
        let suggestion = crate::suggestion_for(src, err);
        let rendered = err.render(src, suggestion.as_deref());
        let lines = rendered
            .lines()
            .map(|l| Line::from(Span::styled(l.to_string(), Style::new().fg(Color::Red))))
            .collect();
        self.output = OutputPane {
            title: "OUTPUT · error".into(),
            lines,
        };
    }

    // ── Editing ──────────────────────────────────────────────────────────────

    fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        let byte = char_to_byte(line, self.cursor_col);
        line.insert(byte, c);
        self.cursor_col += 1;
    }

    fn insert_newline(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let byte = char_to_byte(line, self.cursor_col);
        let rest = line.split_off(byte);
        self.lines.insert(self.cursor_row + 1, rest);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            let prev = char_to_byte(line, self.cursor_col - 1);
            let cur = char_to_byte(line, self.cursor_col);
            line.replace_range(prev..cur, "");
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            let line = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
            self.lines[self.cursor_row].push_str(&line);
        }
    }

    fn move_cursor(&mut self, code: KeyCode) {
        let line_len = |l: &str| l.chars().count();
        match code {
            KeyCode::Left => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                } else if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    self.cursor_col = line_len(&self.lines[self.cursor_row]);
                }
            }
            KeyCode::Right => {
                let len = line_len(&self.lines[self.cursor_row]);
                if self.cursor_col < len {
                    self.cursor_col += 1;
                } else if self.cursor_row + 1 < self.lines.len() {
                    self.cursor_row += 1;
                    self.cursor_col = 0;
                }
            }
            KeyCode::Up if self.cursor_row > 0 => {
                self.cursor_row -= 1;
                self.cursor_col = self.cursor_col.min(line_len(&self.lines[self.cursor_row]));
            }
            KeyCode::Down if self.cursor_row + 1 < self.lines.len() => {
                self.cursor_row += 1;
                self.cursor_col = self.cursor_col.min(line_len(&self.lines[self.cursor_row]));
            }
            _ => {}
        }
    }

    // ── Event handling ───────────────────────────────────────────────────────

    fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Global bindings (work regardless of focus).
        match key.code {
            KeyCode::Char('q') if ctrl => {
                self.quit = true;
                return;
            }
            KeyCode::Char('d') if ctrl => {
                self.show_disassembly();
                return;
            }
            KeyCode::Char('a') if ctrl => {
                self.show_ast();
                return;
            }
            KeyCode::Char('b') if ctrl => {
                self.tab = match self.tab {
                    Tab::Editor => Tab::Build,
                    Tab::Build => Tab::Editor,
                };
                return;
            }
            KeyCode::Char('e') if ctrl => {
                self.tab = Tab::Editor;
                return;
            }
            KeyCode::Enter if ctrl => {
                match self.tab {
                    Tab::Build => self.run_build(),
                    Tab::Editor => self.run_query(),
                }
                return;
            }
            KeyCode::F(5) => {
                self.run_query();
                return;
            }
            KeyCode::F(1) => {
                self.show_help = !self.show_help;
                return;
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Editor => Focus::Output,
                    Focus::Output => Focus::Editor,
                };
                return;
            }
            KeyCode::Esc if self.show_help => {
                self.show_help = false;
                return;
            }
            _ => {}
        }

        if self.show_help {
            return; // overlay swallows other input
        }

        // BUILD tab: arrows pick the target, Space toggles debug/release.
        if self.tab == Tab::Build {
            match key.code {
                KeyCode::Up => self.build_target = self.build_target.saturating_sub(1),
                KeyCode::Down => {
                    self.build_target = (self.build_target + 1).min(BUILD_TARGETS.len() - 1)
                }
                KeyCode::Char(' ') => self.build_release = !self.build_release,
                _ => {}
            }
            return;
        }

        match self.focus {
            Focus::Output => match key.code {
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                _ => {}
            },
            Focus::Editor => match key.code {
                KeyCode::Char(c) => self.insert_char(c),
                KeyCode::Enter => self.insert_newline(),
                KeyCode::Backspace => self.backspace(),
                KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                    self.move_cursor(key.code)
                }
                _ => {}
            },
        }
    }

    // ── Rendering ──────────────────────────────────────────────────────────────

    fn draw(&self, frame: &mut Frame) {
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // header
                Constraint::Min(0),    // body
                Constraint::Length(6), // architecture
            ])
            .split(frame.area());

        self.draw_header(frame, outer[0]);

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(outer[1]);

        match self.tab {
            Tab::Editor => {
                // Split the editor column to show a VARIABLES panel when the
                // query references $vars.
                let vars = self.detected_vars();
                if vars.is_empty() {
                    self.draw_editor(frame, body[0]);
                } else {
                    let left = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Min(3),
                            Constraint::Length((vars.len() + 2).min(8) as u16),
                        ])
                        .split(body[0]);
                    self.draw_editor(frame, left[0]);
                    self.draw_variables(frame, left[1], &vars);
                }
                self.draw_output(frame, body[1]);
            }
            Tab::Build => {
                self.draw_build(frame, body[0]);
                self.draw_output(frame, body[1]);
            }
        }
        self.draw_architecture(frame, outer[2]);

        if self.show_help {
            self.draw_help(frame);
        }
    }

    fn draw_build(&self, frame: &mut Frame, area: Rect) {
        let mut lines = vec![
            Line::from(Span::styled("Target:", Style::new().fg(Color::Gray).bold())),
        ];
        for (i, (label, _, _)) in BUILD_TARGETS.iter().enumerate() {
            let marker = if i == self.build_target { "●" } else { "○" };
            let available = self.target_available(i);
            let selected = i == self.build_target;
            let style = match (selected, available) {
                (true, true) => Style::new().fg(Color::Rgb(166, 227, 161)).bold(),
                (true, false) => Style::new().fg(Color::Rgb(243, 139, 168)).bold(),
                (false, true) => Style::new().fg(Color::Gray),
                (false, false) => Style::new().fg(Color::DarkGray),
            };
            let suffix = if available { "" } else { "  (not installed)" };
            lines.push(Line::from(Span::styled(
                format!("  {marker} {label}{suffix}"),
                style,
            )));
        }
        lines.push(Line::from(""));
        let mode = if self.build_release { "● release   ○ debug" } else { "○ release   ● debug" };
        lines.push(Line::from(vec![
            Span::styled("Mode:   ", Style::new().fg(Color::Gray).bold()),
            Span::styled(mode, Style::new().fg(Color::Rgb(250, 179, 135))),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Output: ", Style::new().fg(Color::Gray).bold()),
            Span::styled(self.build_output_name(), Style::new().fg(Color::Cyan)),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "↑/↓ target · Space debug/release · Ctrl+Enter build · Ctrl+E editor",
            Style::new().fg(Color::DarkGray),
        )));
        frame.render_widget(
            Paragraph::new(lines).block(pane_block("BUILD", true)).wrap(Wrap { trim: false }),
            area,
        );
    }

    fn draw_variables(&self, frame: &mut Frame, area: Rect, vars: &[(String, String)]) {
        let lines: Vec<Line> = vars
            .iter()
            .map(|(name, meta)| {
                Line::from(vec![
                    Span::styled(format!("  ${name:<10}"), Style::new().fg(Color::Rgb(203, 166, 247))),
                    Span::styled(meta.clone(), Style::new().fg(Color::DarkGray)),
                ])
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).block(pane_block("VARIABLES", false)),
            area,
        );
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let tab_style = |active: bool| {
            if active {
                Style::new().fg(Color::Black).bg(Color::Rgb(250, 179, 135)).bold()
            } else {
                Style::new().fg(Color::Gray)
            }
        };
        let title = Line::from(vec![
            Span::styled("CodonSplice", Style::new().fg(Color::Cyan).bold()),
            Span::raw("  │  "),
            Span::styled(" EDITOR ", tab_style(self.tab == Tab::Editor)),
            Span::raw(" "),
            Span::styled(" BUILD ", tab_style(self.tab == Tab::Build)),
            Span::raw("  "),
            Span::styled("(Ctrl+B)  INSTALL→ splice install", Style::new().fg(Color::DarkGray)),
        ]);
        let hints = Line::from(Span::styled(
            "Ctrl+Enter run/build · Ctrl+B build tab · Ctrl+D bytecode · Ctrl+A AST · F1 help · Ctrl+Q quit",
            Style::new().fg(Color::DarkGray),
        ));
        let p = Paragraph::new(vec![title, hints]).block(Block::default().borders(Borders::ALL));
        frame.render_widget(p, area);
    }

    fn draw_editor(&self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Editor;
        let block = pane_block("EDITOR", focused);
        let inner = block.inner(area);
        let text: Vec<Line> = self
            .lines
            .iter()
            .map(|l| Line::from(l.clone()))
            .collect();
        frame.render_widget(Paragraph::new(text).block(block), area);

        if focused && !self.show_help {
            let x = inner.x + self.cursor_col as u16;
            let y = inner.y + self.cursor_row as u16;
            frame.set_cursor_position((x, y));
        }
    }

    fn draw_output(&self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Output;
        let block = pane_block(&self.output.title, focused);
        let p = Paragraph::new(self.output.lines.clone())
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        frame.render_widget(p, area);
    }

    fn draw_architecture(&self, frame: &mut Frame, area: Rect) {
        let arrow = |a: &'static str, b: &'static str| {
            Line::from(vec![
                Span::styled(format!("{a:<22}"), Style::new().fg(Color::Green)),
                Span::styled("→  ", Style::new().fg(Color::DarkGray)),
                Span::styled(b.to_string(), Style::new().fg(Color::Cyan)),
            ])
        };
        let lines = vec![
            arrow("spliceql (language)", "codonsplice (engine)"),
            arrow("Lexer→Parser→AST", "Compiler→Bytecode→VM"),
            arrow("crate: spliceql", "crate: codonsplice-core"),
        ];
        let p = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" ARCHITECTURE "),
        );
        frame.render_widget(p, area);
    }

    fn draw_help(&self, frame: &mut Frame) {
        let area = centered_rect(60, 50, frame.area());
        frame.render_widget(Clear, area);
        let lines = vec![
            Line::from(Span::styled("Keybindings", Style::new().fg(Color::Cyan).bold())),
            Line::from(""),
            help_row("Ctrl+Enter / F5", "compile + run the query"),
            help_row("Ctrl+D", "show disassembled bytecode"),
            help_row("Ctrl+A", "show the parsed AST"),
            help_row("Tab", "switch focus (editor ↔ output)"),
            help_row("F1", "toggle this help"),
            help_row("Ctrl+Q", "quit"),
            Line::from(""),
            Line::from(Span::styled(
                "Esc closes this overlay",
                Style::new().fg(Color::DarkGray),
            )),
        ];
        let p = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Help ")
                .border_style(Style::new().fg(Color::Yellow)),
        );
        frame.render_widget(p, area);
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(Color::Yellow)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(format!(" {title} "))
}

fn help_row(keys: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {keys:<16}"), Style::new().fg(Color::Yellow)),
        Span::raw(desc.to_string()),
    ])
}

/// Syntax-highlight one disassembly line: addresses dim, opcodes cyan,
/// operands yellow, `;` comments dark gray.
fn highlight_disasm(line: &str) -> Line<'static> {
    if line.trim_start().starts_with(';') {
        return Line::from(Span::styled(
            line.to_string(),
            Style::new().fg(Color::DarkGray),
        ));
    }
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Line::from(line.to_string());
    }
    let mut spans = vec![Span::styled(
        format!("{}  ", parts[0]),
        Style::new().fg(Color::White).add_modifier(Modifier::DIM),
    )];
    if parts.len() >= 2 {
        spans.push(Span::styled(
            parts[1].to_string(),
            Style::new().fg(Color::Cyan),
        ));
    }
    if parts.len() >= 3 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            parts[2..].join(" "),
            Style::new().fg(Color::Yellow),
        ));
    }
    Line::from(spans)
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// Byte offset of the `col`-th character in `s` (clamped to the end).
fn char_to_byte(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|(i, _)| i).unwrap_or(s.len())
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Launch the TUI, restoring the terminal on exit.
pub fn run() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal);
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut app = App::new();
    while !app.quit {
        terminal.draw(|frame| app.draw(frame))?;
        // Poll (don't block) so streamed build output + the spinner keep
        // animating while a build runs off-thread.
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.on_key(key);
                }
            }
        }
        app.tick();
    }
    Ok(())
}

/// The rustup targets installed on this machine (`rustup target list
/// --installed`), one triple per line. Empty if rustup isn't available.
fn detect_installed_targets() -> Vec<String> {
    std::process::Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Run `exe build …`, forwarding merged stdout/stderr lines to `tx`. Returns
/// whether the build exited successfully.
fn run_streamed_build(exe: &std::path::Path, args: &[String], tx: &mpsc::Sender<BuildMsg>) -> bool {
    use std::process::{Command, Stdio};
    let mut child = match Command::new(exe)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(BuildMsg::Line(format!("error: {e}")));
            return false;
        }
    };

    let mut handles = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(BuildMsg::Line(line));
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let _ = tx.send(BuildMsg::Line(line));
            }
        }));
    }
    let status = child.wait();
    for h in handles {
        let _ = h.join();
    }
    matches!(status, Ok(s) if s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn build_tab_arrows_change_target() {
        let mut app = App::new();
        app.on_key(ctrl(KeyCode::Char('b'))); // enter BUILD tab
        assert_eq!(app.tab, Tab::Build);
        assert_eq!(app.build_target, 0);

        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Down));
        assert_eq!(app.build_target, 2, "Down should advance the target");

        app.on_key(press(KeyCode::Up));
        assert_eq!(app.build_target, 1, "Up should go back");

        // Clamps at both ends.
        for _ in 0..20 {
            app.on_key(press(KeyCode::Down));
        }
        assert_eq!(app.build_target, BUILD_TARGETS.len() - 1);
        for _ in 0..20 {
            app.on_key(press(KeyCode::Up));
        }
        assert_eq!(app.build_target, 0);
    }

    #[test]
    fn build_tab_space_toggles_release() {
        let mut app = App::new();
        app.on_key(ctrl(KeyCode::Char('b')));
        let before = app.build_release;
        app.on_key(press(KeyCode::Char(' ')));
        assert_eq!(app.build_release, !before);
    }

    #[test]
    fn tick_streams_build_output_and_finishes() {
        let mut app = App::new();
        let (tx, rx) = mpsc::channel();
        app.building = true;
        app.build_rx = Some(rx);
        app.build_label = "native (host)".into();

        tx.send(BuildMsg::Line("Compiling foo".into())).unwrap();
        app.tick();
        assert!(app.building, "still building until Done arrives");
        assert!(app.build_lines.iter().any(|l| l.contains("Compiling foo")));

        tx.send(BuildMsg::Done(true)).unwrap();
        app.tick();
        assert!(!app.building, "Done flips building off");
        assert!(app.build_rx.is_none());
        assert!(app.build_lines.iter().any(|l| l.starts_with('✓')));
    }

    #[test]
    fn run_build_is_noop_while_building() {
        let mut app = App::new();
        app.building = true;
        app.build_lines = vec!["existing".into()];
        app.run_build(); // must not start a second build
        assert_eq!(app.build_lines, vec!["existing".to_string()]);
    }

    #[test]
    fn native_target_always_available() {
        let mut app = App::new();
        app.installed_targets = vec![]; // nothing installed
        assert_eq!(BUILD_TARGETS[0].0, "native (host)");
        assert!(app.target_available(0), "native host needs no rustup target");
    }

    #[test]
    fn cross_target_guarded_when_not_installed() {
        let mut app = App::new();
        app.installed_targets = vec!["x86_64-unknown-linux-gnu".into()];
        // Find the windows cross target (has a triple, not installed here).
        let win = BUILD_TARGETS
            .iter()
            .position(|(_, t, _)| *t == Some("x86_64-pc-windows-msvc"))
            .unwrap();
        assert!(!app.target_available(win));

        app.tab = Tab::Build;
        app.build_target = win;
        app.run_build();
        assert!(!app.building, "an unavailable target must not start a build");
        let text: String = app
            .output
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.clone()))
            .collect();
        assert!(text.contains("rustup target add"), "should show an install hint: {text}");
    }

    #[test]
    fn installed_cross_target_is_available() {
        let mut app = App::new();
        app.installed_targets = vec!["x86_64-pc-windows-msvc".into()];
        let win = BUILD_TARGETS
            .iter()
            .position(|(_, t, _)| *t == Some("x86_64-pc-windows-msvc"))
            .unwrap();
        assert!(app.target_available(win));
    }
}

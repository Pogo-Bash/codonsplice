//! The interactive CodonSplice TUI.
//!
//! Three panes are always visible: an EDITOR (top-left), an OUTPUT pane
//! (top-right), and a fixed ARCHITECTURE panel (bottom) that makes the
//! two-crate design explicit at all times.

use std::io;

use codonsplice_core::{compile, disassemble, Vm, VmError, VmOutput};
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
}

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
                    Err(VmError::NotYetImplemented(op)) => format!(
                        "✓ compiled OK ({bytes} bytes).\n\npipeline execution stubs at `{op}`.\nthe cnvlens-core bridge lands in Phase 4 — for now, try Ctrl+D to\ninspect the bytecode this query lowers to."
                    ),
                    Err(e) => format!("runtime error: {e}"),
                };
                self.output = OutputPane::from_text("OUTPUT · run", &msg);
            }
            Err(e) => self.show_error(&src, &e),
        }
        self.scroll = 0;
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
            KeyCode::Enter if ctrl => {
                self.run_query();
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

        self.draw_editor(frame, body[0]);
        self.draw_output(frame, body[1]);
        self.draw_architecture(frame, outer[2]);

        if self.show_help {
            self.draw_help(frame);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let title = Line::from(vec![
            Span::styled("CodonSplice", Style::new().fg(Color::Cyan).bold()),
            Span::raw("  │  "),
            Span::styled("SpliceQL query engine", Style::new().fg(Color::Gray)),
        ]);
        let hints = Line::from(Span::styled(
            "Ctrl+Enter/F5 run · Ctrl+D bytecode · Ctrl+A AST · Tab focus · F1 help · Ctrl+Q quit",
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
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                app.on_key(key);
            }
        }
    }
    Ok(())
}

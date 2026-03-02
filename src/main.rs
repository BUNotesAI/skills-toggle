use std::fs;
use std::io;
use std::path::PathBuf;

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

// ── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "skills-toggle", about = "TUI tool for enabling/disabling Claude Code skills")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// List all skills and their status (non-interactive)
    #[arg(long)]
    list: bool,

    /// Show what would change without executing
    #[arg(long)]
    dry_run: bool,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Batch-enable skills by name or glob pattern (e.g. "actix-*")
    Enable {
        /// Skill names or glob patterns
        #[arg(required = true)]
        patterns: Vec<String>,

        /// Show what would change without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Batch-disable skills by name or glob pattern (e.g. "actix-*")
    Disable {
        /// Skill names or glob patterns
        #[arg(required = true)]
        patterns: Vec<String>,

        /// Show what would change without executing
        #[arg(long)]
        dry_run: bool,
    },
}

/// Simple glob matching: supports `*` (any chars) and `?` (single char)
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.to_lowercase().chars().collect();
    let txt: Vec<char> = text.to_lowercase().chars().collect();
    let (plen, tlen) = (pat.len(), txt.len());
    // dp[i][j] = pattern[..i] matches text[..j]
    let mut dp = vec![vec![false; tlen + 1]; plen + 1];
    dp[0][0] = true;
    // Leading *s can match empty
    for i in 1..=plen {
        if pat[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=plen {
        for j in 1..=tlen {
            if pat[i - 1] == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if pat[i - 1] == '?' || pat[i - 1] == txt[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[plen][tlen]
}

// ── Skill data ──────────────────────────────────────────────────────────────

struct Skill {
    name: String,
    /// true = currently on disk in skills dir (enabled)
    enabled_on_disk: bool,
    /// working state in TUI
    toggled: bool,
}

impl Skill {
    fn changed(&self) -> bool {
        self.enabled_on_disk != self.toggled
    }
}

// ── App state ───────────────────────────────────────────────────────────────

enum Mode {
    Normal,
    Filter,
    Confirm,
}

struct App {
    skills: Vec<Skill>,
    /// Indices into `skills` that match the current filter
    visible: Vec<usize>,
    list_state: ListState,
    filter: String,
    mode: Mode,
    skills_dir: PathBuf,
    disabled_dir: PathBuf,
    dry_run: bool,
    should_quit: bool,
    result_message: Option<String>,
    /// scroll offset for confirm panel when content overflows
    confirm_scroll: usize,
}

impl App {
    fn new(skills_dir: PathBuf, dry_run: bool) -> io::Result<Self> {
        let disabled_dir = skills_dir.join(".disabled");
        fs::create_dir_all(&disabled_dir)?;

        let mut skills = Vec::new();

        // Scan enabled skills
        if let Ok(entries) = fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || !entry.path().is_dir() {
                    continue;
                }
                skills.push(Skill {
                    name,
                    enabled_on_disk: true,
                    toggled: true,
                });
            }
        }

        // Scan disabled skills
        if let Ok(entries) = fs::read_dir(&disabled_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || !entry.path().is_dir() {
                    continue;
                }
                skills.push(Skill {
                    name,
                    enabled_on_disk: false,
                    toggled: false,
                });
            }
        }

        // Checked (enabled) first, then unchecked (disabled); alphabetical within each group
        skills.sort_by(|a, b| {
            b.toggled
                .cmp(&a.toggled)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        let visible: Vec<usize> = (0..skills.len()).collect();

        let mut list_state = ListState::default();
        if !visible.is_empty() {
            list_state.select(Some(0));
        }

        Ok(App {
            skills,
            visible,
            list_state,
            filter: String::new(),
            mode: Mode::Normal,
            skills_dir,
            disabled_dir,
            dry_run,
            should_quit: false,
            result_message: None,
            confirm_scroll: 0,
        })
    }

    fn total(&self) -> usize {
        self.skills.len()
    }

    fn enabled_count(&self) -> usize {
        self.skills.iter().filter(|s| s.toggled).count()
    }

    fn disabled_count(&self) -> usize {
        self.skills.iter().filter(|s| !s.toggled).count()
    }

    fn changed_count(&self) -> usize {
        self.skills.iter().filter(|s| s.changed()).count()
    }

    fn selected_idx(&self) -> Option<usize> {
        self.list_state.selected().and_then(|i| self.visible.get(i).copied())
    }

    // ── Filter ──

    fn apply_filter(&mut self) {
        let lower = self.filter.to_lowercase();
        self.visible = (0..self.skills.len())
            .filter(|&i| {
                lower.is_empty() || self.skills[i].name.to_lowercase().contains(&lower)
            })
            .collect();

        // Keep cursor in bounds
        let sel = self.list_state.selected().unwrap_or(0);
        if self.visible.is_empty() {
            self.list_state.select(None);
        } else if sel >= self.visible.len() {
            self.list_state.select(Some(self.visible.len() - 1));
        }
    }

    // ── Toggle ──

    fn toggle_current(&mut self) {
        if let Some(idx) = self.selected_idx() {
            self.skills[idx].toggled = !self.skills[idx].toggled;
            // Auto-advance
            if let Some(sel) = self.list_state.selected() {
                if sel + 1 < self.visible.len() {
                    self.list_state.select(Some(sel + 1));
                }
            }
        }
    }

    fn set_all_visible(&mut self, value: bool) {
        for &idx in &self.visible {
            self.skills[idx].toggled = value;
        }
    }

    // ── Navigation ──

    fn move_up(&mut self) {
        if let Some(sel) = self.list_state.selected() {
            if sel > 0 {
                self.list_state.select(Some(sel - 1));
            }
        }
    }

    fn move_down(&mut self) {
        if let Some(sel) = self.list_state.selected() {
            if sel + 1 < self.visible.len() {
                self.list_state.select(Some(sel + 1));
            }
        }
    }

    fn page_up(&mut self, page_size: usize) {
        if let Some(sel) = self.list_state.selected() {
            self.list_state.select(Some(sel.saturating_sub(page_size)));
        }
    }

    fn page_down(&mut self, page_size: usize) {
        if let Some(sel) = self.list_state.selected() {
            let max = self.visible.len().saturating_sub(1);
            self.list_state.select(Some((sel + page_size).min(max)));
        }
    }

    fn go_top(&mut self) {
        if !self.visible.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    fn go_bottom(&mut self) {
        if !self.visible.is_empty() {
            self.list_state.select(Some(self.visible.len() - 1));
        }
    }

    // ── Apply ──

    fn changes_to_disable(&self) -> Vec<&str> {
        self.skills
            .iter()
            .filter(|s| s.changed() && !s.toggled)
            .map(|s| s.name.as_str())
            .collect()
    }

    fn changes_to_enable(&self) -> Vec<&str> {
        self.skills
            .iter()
            .filter(|s| s.changed() && s.toggled)
            .map(|s| s.name.as_str())
            .collect()
    }

    fn collect_moves(&self) -> Vec<(PathBuf, PathBuf)> {
        self.skills
            .iter()
            .filter(|s| s.changed())
            .map(|s| {
                if !s.toggled {
                    (self.skills_dir.join(&s.name), self.disabled_dir.join(&s.name))
                } else {
                    (self.disabled_dir.join(&s.name), self.skills_dir.join(&s.name))
                }
            })
            .collect()
    }

    fn apply_changes(&mut self) -> BatchResult {
        let moves = self.collect_moves();
        if self.dry_run {
            for (src, dst) in &moves {
                println!("  mv '{}' '{}'", src.display(), dst.display());
            }
            BatchResult { applied: moves.len(), rolled_back: 0, failed: false }
        } else {
            atomic_batch_move(&moves)
        }
    }
}

// ── Atomic batch move ───────────────────────────────────────────────────────

struct BatchResult {
    applied: usize,
    rolled_back: usize,
    failed: bool,
}

/// Execute a batch of renames atomically: all succeed or all are rolled back.
///
/// 1. Pre-validate: check every src exists and every dst does not.
/// 2. Execute moves one by one, recording successes.
/// 3. On any failure, reverse all successful moves in LIFO order.
fn atomic_batch_move(moves: &[(PathBuf, PathBuf)]) -> BatchResult {
    // Phase 1: pre-validate
    for (src, dst) in moves {
        if !src.exists() {
            eprintln!("  \x1b[33mabort\x1b[0m source missing: {}", src.display());
            return BatchResult { applied: 0, rolled_back: 0, failed: true };
        }
        if dst.exists() {
            eprintln!("  \x1b[33mabort\x1b[0m destination already exists: {}", dst.display());
            return BatchResult { applied: 0, rolled_back: 0, failed: true };
        }
    }

    // Phase 2: execute, recording completed moves for rollback
    let mut done: Vec<(&PathBuf, &PathBuf)> = Vec::new();

    for (src, dst) in moves {
        if let Err(e) = fs::rename(src, dst) {
            eprintln!("  \x1b[31mfailed\x1b[0m {} → {}: {e}", src.display(), dst.display());

            // Phase 3: rollback in reverse order
            let mut rollback_errors = 0;
            for (orig_src, orig_dst) in done.iter().rev() {
                // Reverse: move dst back to src
                if let Err(re) = fs::rename(orig_dst, orig_src) {
                    rollback_errors += 1;
                    eprintln!(
                        "  \x1b[31mrollback failed\x1b[0m {} → {}: {re}",
                        orig_dst.display(),
                        orig_src.display()
                    );
                }
            }

            let rolled_back = done.len() - rollback_errors;
            return BatchResult { applied: 0, rolled_back, failed: true };
        }
        done.push((src, dst));
    }

    BatchResult { applied: done.len(), rolled_back: 0, failed: false }
}

// ── Non-interactive --list ──────────────────────────────────────────────────

fn run_list(app: &App) {
    let green = "\x1b[32m";
    let red = "\x1b[31m";
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";

    for skill in &app.skills {
        if skill.enabled_on_disk {
            println!("{green}[x]{reset} {}", skill.name);
        } else {
            println!("{red}[ ]{reset} {}", skill.name);
        }
    }
    println!();
    println!(
        "{bold}Enabled:{reset} {}  {bold}Disabled:{reset} {}  {bold}Total:{reset} {}",
        app.enabled_count(),
        app.disabled_count(),
        app.total()
    );
}

// ── TUI rendering ───────────────────────────────────────────────────────────

fn ui(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Layout: header, list, filter (optional), status, help
    let has_filter = !app.filter.is_empty() || matches!(app.mode, Mode::Filter);
    let constraints = if has_filter {
        vec![
            Constraint::Length(2),  // header
            Constraint::Min(3),     // list
            Constraint::Length(1),  // filter
            Constraint::Length(1),  // status
            Constraint::Length(1),  // help
        ]
    } else {
        vec![
            Constraint::Length(2),  // header
            Constraint::Min(3),     // list
            Constraint::Length(1),  // status
            Constraint::Length(1),  // help
        ]
    };

    let chunks = Layout::vertical(constraints).split(area);

    // ── Header ──
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " Claude Code Skills Manager",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    frame.render_widget(header, chunks[0]);

    // ── List ──
    let list_area = chunks[1];
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|&idx| {
            let skill = &app.skills[idx];
            let checkbox = if skill.toggled {
                Span::styled("[x]", Style::default().fg(Color::Green))
            } else {
                Span::styled("[ ]", Style::default().fg(Color::Red))
            };
            let name = Span::raw(format!(" {}", skill.name));
            let marker = if skill.changed() {
                Span::styled(" *", Style::default().fg(Color::Yellow))
            } else {
                Span::raw("")
            };
            ListItem::new(Line::from(vec![checkbox, name, marker]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::REVERSED)
        )
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, list_area, &mut app.list_state);

    // ── Filter line ──
    let status_chunk;
    let help_chunk;

    if has_filter {
        let filter_area = chunks[2];
        status_chunk = chunks[3];
        help_chunk = chunks[4];

        let filter_text = if matches!(app.mode, Mode::Filter) {
            Line::from(vec![
                Span::styled(" / ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(&app.filter),
                Span::styled("▌", Style::default().add_modifier(Modifier::BOLD)),
            ])
        } else {
            Line::from(vec![
                Span::styled(" Filter: ", Style::default().add_modifier(Modifier::DIM)),
                Span::raw(&app.filter),
                Span::styled("  (/ to edit, Esc to clear)", Style::default().add_modifier(Modifier::DIM)),
            ])
        };
        frame.render_widget(Paragraph::new(filter_text), filter_area);
    } else {
        status_chunk = chunks[2];
        help_chunk = chunks[3];
    }

    // ── Status bar ──
    let pos = app
        .list_state
        .selected()
        .map(|s| format!("  [{}/{}]", s + 1, app.visible.len()))
        .unwrap_or_default();

    let status = Line::from(vec![
        Span::styled(" Enabled: ", Style::default().fg(Color::Green)),
        Span::raw(app.enabled_count().to_string()),
        Span::styled("  |  ", Style::default().add_modifier(Modifier::DIM)),
        Span::styled("Disabled: ", Style::default().fg(Color::Red)),
        Span::raw(app.disabled_count().to_string()),
        Span::styled("  |  ", Style::default().add_modifier(Modifier::DIM)),
        Span::styled("Changed: ", Style::default().fg(Color::Yellow)),
        Span::raw(app.changed_count().to_string()),
        Span::styled(pos, Style::default().add_modifier(Modifier::DIM)),
    ]);
    frame.render_widget(Paragraph::new(status), status_chunk);

    // ── Help bar ──
    let help = Line::from(vec![
        Span::styled(" ↑/↓", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Navigate  "),
        Span::styled("Space", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Toggle  "),
        Span::styled("a", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":All  "),
        Span::styled("n", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":None  "),
        Span::styled("/", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Filter  "),
        Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Apply  "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Quit"),
    ]);
    frame.render_widget(Paragraph::new(help), help_chunk);

    // ── Confirm panel overlay ──
    if matches!(app.mode, Mode::Confirm) {
        render_confirm(frame, app, area);
    }
}

fn render_confirm(frame: &mut Frame, app: &App, area: Rect) {
    let to_disable = app.changes_to_disable();
    let to_enable = app.changes_to_enable();

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));

    if !to_disable.is_empty() {
        lines.push(Line::styled(
            format!("  Will DISABLE {} skill(s):", to_disable.len()),
            Style::default().fg(Color::Red),
        ));
        for name in &to_disable {
            lines.push(Line::styled(
                format!("    {name}"),
                Style::default().fg(Color::Red),
            ));
        }
    }

    if !to_enable.is_empty() {
        if !to_disable.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            format!("  Will ENABLE {} skill(s):", to_enable.len()),
            Style::default().fg(Color::Green),
        ));
        for name in &to_enable {
            lines.push(Line::styled(
                format!("    {name}"),
                Style::default().fg(Color::Green),
            ));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  Enter: apply  |  Esc: go back  |  ↑/↓: scroll",
        Style::default().add_modifier(Modifier::DIM),
    ));

    // Size the panel
    let content_height = lines.len() as u16 + 2; // +2 for border
    let max_width = lines
        .iter()
        .map(|l| l.width())
        .max()
        .unwrap_or(30)
        .max(30) as u16
        + 4; // +4 for border + padding

    let panel_w = max_width.min(area.width.saturating_sub(4));
    let panel_h = content_height.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(panel_w)) / 2;
    let y = area.y + (area.height.saturating_sub(panel_h)) / 2;
    let panel_rect = Rect::new(x, y, panel_w, panel_h);

    frame.render_widget(Clear, panel_rect);

    let confirm = Paragraph::new(lines)
        .scroll((app.confirm_scroll as u16, 0))
        .block(
            Block::default()
                .title(" Confirm Changes ")
                .title_style(Style::default().add_modifier(Modifier::BOLD))
                .borders(Borders::ALL),
        );
    frame.render_widget(confirm, panel_rect);
}

// ── Event handling ──────────────────────────────────────────────────────────

fn handle_event(app: &mut App, viewport_height: u16) -> io::Result<()> {
    if !event::poll(std::time::Duration::from_millis(100))? {
        return Ok(());
    }

    let Event::Key(key) = event::read()? else {
        return Ok(());
    };

    // Only handle key press events (not release/repeat)
    if key.kind != KeyEventKind::Press {
        return Ok(());
    }

    // Ctrl+C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return Ok(());
    }

    match app.mode {
        Mode::Confirm => match key.code {
            KeyCode::Enter => {
                let result = app.apply_changes();
                if app.dry_run {
                    app.result_message = Some("(No changes were made — dry run)".to_string());
                } else if result.failed {
                    let msg = if result.rolled_back > 0 {
                        format!("Failed — rolled back {0} move(s), no changes were made", result.rolled_back)
                    } else {
                        "Failed — no changes were made".to_string()
                    };
                    app.result_message = Some(msg);
                } else {
                    let label = if result.applied == 1 { "change" } else { "changes" };
                    app.result_message =
                        Some(format!("Successfully applied {} {label}", result.applied));
                }
                app.should_quit = true;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                app.mode = Mode::Normal;
                app.confirm_scroll = 0;
            }
            KeyCode::Up => {
                app.confirm_scroll = app.confirm_scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                app.confirm_scroll += 1;
            }
            _ => {}
        },

        Mode::Filter => match key.code {
            KeyCode::Enter | KeyCode::Esc => {
                app.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                app.filter.pop();
                app.apply_filter();
            }
            KeyCode::Up | KeyCode::Down => {
                app.mode = Mode::Normal;
                // Fall through to handle as navigation
                if key.code == KeyCode::Up {
                    app.move_up();
                } else {
                    app.move_down();
                }
            }
            KeyCode::Char(c) => {
                app.filter.push(c);
                app.apply_filter();
            }
            _ => {}
        },

        Mode::Normal => {
            let page = viewport_height.saturating_sub(4) as usize;
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    if !app.filter.is_empty() && key.code == KeyCode::Esc {
                        app.filter.clear();
                        app.apply_filter();
                    } else {
                        app.should_quit = true;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                KeyCode::PageUp => app.page_up(page),
                KeyCode::PageDown => app.page_down(page),
                KeyCode::Char('g') => app.go_top(),
                KeyCode::Char('G') => app.go_bottom(),
                KeyCode::Char(' ') => app.toggle_current(),
                KeyCode::Char('a') => app.set_all_visible(true),
                KeyCode::Char('n') => app.set_all_visible(false),
                KeyCode::Char('/') => {
                    app.mode = Mode::Filter;
                }
                KeyCode::Enter => {
                    if app.changed_count() > 0 {
                        app.mode = Mode::Confirm;
                        app.confirm_scroll = 0;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

// ── Main loop ───────────────────────────────────────────────────────────────

fn run_tui(mut app: App) -> io::Result<Option<String>> {
    let mut terminal = ratatui::init();

    let result = (|| -> io::Result<Option<String>> {
        loop {
            let viewport_height = terminal.get_frame().area().height;
            terminal.draw(|f| ui(f, &mut app))?;
            handle_event(&mut app, viewport_height)?;
            if app.should_quit {
                break;
            }
        }
        Ok(app.result_message.take())
    })();

    ratatui::restore();
    result
}

// ── Batch enable/disable ────────────────────────────────────────────────────

fn run_batch(skills_dir: PathBuf, patterns: &[String], enable: bool, dry_run: bool) -> io::Result<()> {
    let disabled_dir = skills_dir.join(".disabled");
    fs::create_dir_all(&disabled_dir)?;

    let green = "\x1b[32m";
    let red = "\x1b[31m";
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";

    // Collect all skill names and their current state
    let mut all_skills: Vec<(String, bool)> = Vec::new(); // (name, currently_enabled)

    if let Ok(entries) = fs::read_dir(&skills_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !entry.path().is_dir() {
                continue;
            }
            all_skills.push((name, true));
        }
    }
    if let Ok(entries) = fs::read_dir(&disabled_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !entry.path().is_dir() {
                continue;
            }
            all_skills.push((name, false));
        }
    }
    all_skills.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

    // Find skills matching any pattern
    let mut matched: Vec<(String, bool)> = Vec::new();
    for (name, currently_enabled) in &all_skills {
        for pattern in patterns {
            if glob_match(pattern, name) {
                matched.push((name.clone(), *currently_enabled));
                break;
            }
        }
    }

    if matched.is_empty() {
        eprintln!("No skills matched the pattern(s): {}", patterns.join(", "));
        std::process::exit(1);
    }

    // Separate into moves and skips
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut move_names: Vec<String> = Vec::new();
    let mut skipped = 0;

    for (name, currently_enabled) in &matched {
        let already_correct = if enable { *currently_enabled } else { !*currently_enabled };

        if already_correct {
            println!("  {dim}skip{reset} {name} (already {})", if enable { "enabled" } else { "disabled" });
            skipped += 1;
            continue;
        }

        let (src, dst) = if enable {
            (disabled_dir.join(name), skills_dir.join(name))
        } else {
            (skills_dir.join(name), disabled_dir.join(name))
        };

        if dry_run {
            println!("  {dim}would mv{reset} '{}' → '{}'", src.display(), dst.display());
        }

        moves.push((src, dst));
        move_names.push(name.clone());
    }

    println!();

    if dry_run {
        println!("{dim}Dry run: {} would change, {skipped} already correct{reset}", moves.len());
        return Ok(());
    }

    if moves.is_empty() {
        println!("{dim}Nothing to do ({skipped} already correct){reset}");
        return Ok(());
    }

    // Atomic batch move
    let result = atomic_batch_move(&moves);

    if result.failed {
        if result.rolled_back > 0 {
            eprintln!("{red}Failed — rolled back {} move(s), no changes were made{reset}", result.rolled_back);
        } else {
            eprintln!("{red}Failed — no changes were made{reset}");
        }
        std::process::exit(1);
    }

    // Print each successful move
    for name in &move_names {
        let tag = if enable {
            format!("{green}enabled{reset}")
        } else {
            format!("{red}disabled{reset}")
        };
        println!("  {tag} {name}");
    }

    let label = if result.applied == 1 { "change" } else { "changes" };
    println!("{green}Applied {} {label}{reset}, {skipped} skipped", result.applied);

    Ok(())
}

// ── Entry point ─────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    let skills_dir = dirs::home_dir()
        .expect("Cannot determine home directory")
        .join(".claude")
        .join("skills");

    if !skills_dir.exists() {
        eprintln!("Skills directory not found: {}", skills_dir.display());
        std::process::exit(1);
    }

    // Handle subcommands first
    if let Some(cmd) = cli.command {
        return match cmd {
            Command::Enable { patterns, dry_run } => run_batch(skills_dir, &patterns, true, dry_run),
            Command::Disable { patterns, dry_run } => run_batch(skills_dir, &patterns, false, dry_run),
        };
    }

    let app = App::new(skills_dir, cli.dry_run)?;

    if cli.list {
        run_list(&app);
        return Ok(());
    }

    if app.total() == 0 {
        eprintln!("No skills found");
        std::process::exit(1);
    }

    if cli.dry_run {
        println!("Dry run — commands that would be executed:");
    }

    match run_tui(app)? {
        Some(msg) => println!("{msg}"),
        None => {} // quit without changes
    }

    Ok(())
}

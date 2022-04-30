use crate::poller::StatsReceiver;
use crate::stats::{ConnectionStats, Stats, StatsValue};
use clap::Parser;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
pub struct UiOpts {
    #[clap(long, default_value_t = 600.0)]
    pub retention_period: f64,

    #[clap(long, default_value = "total=.*:.*")]
    pub tab: Vec<Tab>,
}

#[derive(Debug, Clone)]
pub struct Tab {
    name: String,
    key_regex: regex::Regex,
    value_regex: regex::Regex,
}

impl Tab {
    pub fn is_match(&self, stats: &Stats) -> bool {
        stats
            .iter()
            .any(|(k, v)| self.key_regex.is_match(k) && self.value_regex.is_match(&v.to_string()))
    }
}

impl std::fmt::Display for Tab {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}={}:{}", self.name, self.key_regex, self.value_regex)
    }
}

impl std::str::FromStr for Tab {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let [name, rest] = s.splitn(2, '=').collect::<Vec<_>>().as_slice() {
            if let [k, v] = rest.splitn(2, ':').collect::<Vec<_>>().as_slice() {
                return Ok(Self {
                    name: name.to_string(),
                    key_regex: regex::Regex::new(k)?,
                    value_regex: regex::Regex::new(v)?,
                });
            }
        }
        anyhow::bail!(
            "invalid tab spec {s:?} (expected format: \"$NAME=$KEY_REGEX:$VALUE_REGEX\")"
        );
    }
}

type Terminal = tui::Terminal<tui::backend::CrosstermBackend<std::io::Stdout>>;

type Frame<'a> = tui::Frame<'a, tui::backend::CrosstermBackend<std::io::Stdout>>;

// TODO: rename
#[derive(Debug)]
pub struct Ui {
    opt: UiOpts,
    history: VecDeque<HistoryItem>,
    tab_index: usize,
    table_state: tui::widgets::TableState,
}

impl Ui {
    fn new(opt: UiOpts) -> Self {
        Self {
            opt,
            history: VecDeque::new(),
            tab_index: 0,
            table_state: Default::default(),
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        use tui::layout::{Constraint, Direction, Layout};

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Min(0),
                    Constraint::Length(5),
                ]
                .as_ref(),
            )
            .split(f.size());

        self.draw_tabs(f, chunks[0]);
        self.draw_stats(f, chunks[1], self.opt.tab[self.tab_index].clone());
        self.draw_help(f, chunks[2]);
    }

    fn draw_stats(&mut self, f: &mut Frame, area: tui::layout::Rect, tab: Tab) {
        use tui::layout::{Constraint, Direction, Layout};

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(area);

        self.draw_aggregated_stats(f, chunks[0], &tab);
        self.draw_detailed_stats(f, chunks[1], &tab);
    }

    fn latest_stats(&self, tab: &Tab) -> Vec<StatsItem> {
        let mut items = BTreeMap::<_, StatsItem>::new();
        for conn in &self.history.back().expect("unreachable").connections {
            if tab.is_match(&conn.stats) {
                for (k, v) in &conn.stats {
                    let entry = items.entry(k).or_default();
                    entry.key = k.clone();
                    entry.values.insert(v.clone());
                }
            }
        }
        items.into_iter().map(|(_, v)| v).collect()
    }

    fn draw_aggregated_stats(&mut self, f: &mut Frame, area: tui::layout::Rect, tab: &Tab) {
        use tui::layout::Constraint;
        use tui::style::{Color, Modifier, Style};
        use tui::widgets::{Block, Borders, Cell, Row, Table};

        let selected_style = Style::default().add_modifier(Modifier::REVERSED);
        let normal_style = Style::default().bg(Color::Blue);

        let header_cells = ["Key", "Sum", "Uniq"]
            .into_iter()
            .map(|h| Cell::from(h).style(Style::default().fg(Color::Red)));
        let header = Row::new(header_cells)
            .style(normal_style)
            .height(1)
            .bottom_margin(1);

        let items = self.latest_stats(tab);
        let rows = items.into_iter().map(|item| {
            let cells = match item.aggregated_value() {
                Ok(sum) => {
                    vec![
                        Cell::from(item.key),
                        Cell::from(sum.to_string()),
                        Cell::from(""),
                    ]
                }
                Err(uniq) => {
                    vec![
                        Cell::from(item.key),
                        Cell::from(""),
                        Cell::from(uniq.to_string()),
                    ]
                }
            };
            Row::new(cells)
        });

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Aggregated Stats"); // TODO: N connections

        // TODO: align
        let t = Table::new(rows)
            .header(header)
            .block(block)
            .highlight_style(selected_style)
            .highlight_symbol(">> ")
            .widths(&[
                Constraint::Percentage(70),
                Constraint::Percentage(15),
                Constraint::Percentage(15),
            ]);
        f.render_stateful_widget(t, area, &mut self.table_state);
    }

    fn draw_detailed_stats(&mut self, f: &mut Frame, area: tui::layout::Rect, tab: &Tab) {
        use tui::widgets::{Block, Borders};

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Detailed Stats");
        f.render_widget(block, area);
    }

    fn draw_tabs(&mut self, f: &mut Frame, area: tui::layout::Rect) {
        use tui::style::{Color, Modifier, Style};
        use tui::text::Spans;
        use tui::widgets::{Block, Borders, Tabs};

        let tabs = Tabs::new(
            self.opt
                .tab
                .iter()
                .map(|t| Spans::from(t.name.clone()))
                .collect::<Vec<_>>(),
        )
        .select(self.tab_index)
        .block(Block::default().borders(Borders::ALL).title("Tab"))
        .style(Style::default().fg(Color::Cyan))
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::Black),
        );
        f.render_widget(tabs, area);
    }

    fn draw_help(&mut self, f: &mut Frame, area: tui::layout::Rect) {
        use tui::widgets::{Block, Borders};

        let block = Block::default().borders(Borders::ALL).title("Help");
        f.render_widget(block, area);
    }
}

pub struct App {
    rx: StatsReceiver,
    terminal: Terminal,
    ui: Ui,
}

impl App {
    pub fn new(rx: StatsReceiver, opt: UiOpts) -> anyhow::Result<Self> {
        let terminal = Self::setup_terminal()?;
        log::debug!("setup terminal");
        Ok(Self {
            rx,
            ui: Ui::new(opt),
            terminal,
        })
    }

    pub fn run(mut self) -> anyhow::Result<()> {
        loop {
            if self.handle_key_event()? {
                break;
            }
            self.handle_stats_poll()?;
        }
        Ok(())
    }

    fn handle_key_event(&mut self) -> anyhow::Result<bool> {
        if crossterm::event::poll(std::time::Duration::from_secs(0))? {
            // TODO: handle resize event
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                use crossterm::event::KeyCode;
                match key.code {
                    KeyCode::Char('q') => {
                        return Ok(true);
                    }
                    KeyCode::Right => {
                        let tab_index =
                            std::cmp::min(self.ui.tab_index + 1, self.ui.opt.tab.len() - 1);
                        if tab_index != self.ui.tab_index {
                            self.ui.tab_index = tab_index;
                            self.terminal.draw(|f| self.ui.draw(f))?;
                        }
                    }
                    KeyCode::Left => {
                        let tab_index = self.ui.tab_index.saturating_sub(1);
                        if tab_index != self.ui.tab_index {
                            self.ui.tab_index = tab_index;
                            self.terminal.draw(|f| self.ui.draw(f))?;
                        }
                    }
                    KeyCode::Up => {
                        let i = if let Some(i) = self.ui.table_state.selected() {
                            i.saturating_sub(1)
                        } else {
                            0
                        };
                        self.ui.table_state.select(Some(i));
                        self.terminal.draw(|f| self.ui.draw(f))?;
                    }
                    KeyCode::Down => {
                        let i = if let Some(i) = self.ui.table_state.selected() {
                            // TODO: min
                            i + 1
                        } else {
                            0
                        };
                        self.ui.table_state.select(Some(i));
                        self.terminal.draw(|f| self.ui.draw(f))?;
                    }
                    _ => {}
                }
            }
        }
        Ok(false)
    }

    fn handle_stats_poll(&mut self) -> anyhow::Result<()> {
        match self.rx.recv_timeout(Duration::from_millis(10)) {
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("Sora stats polling thread terminated unexpectedly");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Ok(connections) => {
                log::debug!("recv new stats");
                self.ui.history.push_back(HistoryItem {
                    timestamp: Instant::now(),
                    connections,
                });
                while let Some(item) = self.ui.history.pop_front() {
                    if item.timestamp.elapsed().as_secs_f64() < self.ui.opt.retention_period {
                        self.ui.history.push_front(item);
                        break;
                    }
                    log::debug!("remove old stats");
                }
                self.terminal.draw(|f| self.ui.draw(f))?;
            }
        }
        Ok(())
    }

    fn setup_terminal() -> anyhow::Result<Terminal> {
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(
            stdout,
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::EnableMouseCapture
        )?;
        let backend = tui::backend::CrosstermBackend::new(stdout);
        let terminal = tui::Terminal::new(backend)?;
        Ok(terminal)
    }

    fn teardown_terminal(&mut self) -> anyhow::Result<()> {
        crossterm::terminal::disable_raw_mode()?;
        crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        )?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Err(e) = self.teardown_terminal() {
            log::warn!("failed to tear down terminal: {e}");
        } else {
            log::debug!("tear down terminal");
        }
    }
}

#[derive(Debug)]
pub struct HistoryItem {
    timestamp: Instant,
    connections: Vec<ConnectionStats>,
}

#[derive(Debug, Default)]
pub struct StatsItem {
    key: String,
    values: HashSet<StatsValue>,
}

impl StatsItem {
    pub fn aggregated_value(&self) -> Result<f64, usize> {
        let mut sum = 0.0;
        for v in &self.values {
            if let StatsValue::Number(v) = v {
                sum += v.0;
            } else {
                return Err(self.values.len());
            }
        }
        Ok(sum)
    }
}

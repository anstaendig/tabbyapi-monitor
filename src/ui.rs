// Dashboard arrangement follows bryanvine/vllmstat (see THIRD_PARTY_NOTICES.md).
use crate::{
    App, Runtime,
    metrics::{Snapshot, State, ratio},
};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Widget, Wrap},
};
const BASE: Color = Color::Rgb(30, 30, 46);
const SURFACE: Color = Color::Rgb(49, 50, 68);
const TEXT: Color = Color::Rgb(205, 214, 244);
const MAUVE: Color = Color::Rgb(203, 166, 247);
const GREEN: Color = Color::Rgb(166, 227, 161);
const RED: Color = Color::Rgb(243, 139, 168);
const SUB: Color = Color::Rgb(166, 173, 200);
pub fn num(v: Option<f64>) -> String {
    v.map(|n| format!("{n:.1}")).unwrap_or_else(|| "—".into())
}
fn count(v: Option<f64>) -> String {
    v.map(|n| format!("{n:.0}")).unwrap_or_else(|| "—".into())
}
fn compact(v: Option<f64>) -> String {
    match v {
        Some(n) if n.abs() >= 1e6 => format!("{:.1}M", n / 1e6),
        Some(n) if n.abs() >= 1000. => format!("{:.1}k", n / 1000.),
        _ => num(v),
    }
}
fn duration(v: Option<f64>) -> String {
    match v {
        Some(n) if n < 1. => format!("{:.0}ms", n * 1000.),
        Some(n) => format!("{n:.1}s"),
        None => "—".into(),
    }
}
fn heading(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_owned(),
        Style::default().fg(MAUVE).add_modifier(Modifier::BOLD),
    ))
}
// Labelled rows share one label column so values start at the same position.
const LABEL: usize = 8;
const INLINE_LABEL: usize = 13;
fn field(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:LABEL$} "), Style::default().fg(SUB)),
        Span::raw(value),
    ])
}
fn inline(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:INLINE_LABEL$}"), Style::default().fg(MAUVE)),
        Span::raw(value),
    ])
}
fn panel(buf: &mut Buffer, area: Rect, lines: Vec<Line<'static>>) {
    Paragraph::new(lines)
        .style(Style::default().fg(TEXT).bg(BASE))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(MAUVE))
                .padding(Padding::horizontal(1)),
        )
        .wrap(Wrap { trim: false })
        .render(area, buf);
}
fn panel_height(lines: &[Line<'_>], width: u16) -> u16 {
    let inner = width.saturating_sub(4).max(1) as usize;
    2 + lines
        .iter()
        .map(|l| l.width().max(1).div_ceil(inner) as u16)
        .sum::<u16>()
}
#[derive(Clone, Copy)]
enum Series {
    Running,
    Waiting,
    Generation,
    Prompt,
}
fn chart(state: &State, now: f64, width: usize, series: Series) -> Vec<Line<'static>> {
    let width = width.clamp(1, 240);
    let recent: Vec<_> = state
        .history
        .iter()
        .filter(|p| now - p.time <= 120.)
        .collect();
    // All charts share one time axis starting at the first rate sample (the first
    // poll has no rate), so side-by-side charts line up column for column.
    let start = recent
        .iter()
        .position(|p| p.output.or(p.computed).is_some())
        .or_else(|| recent.iter().position(|p| p.running.or(p.queued).is_some()))
        .unwrap_or(recent.len());
    let values: Vec<(f64, Option<f64>)> = recent[start..]
        .iter()
        .map(|p| {
            let v = match series {
                Series::Running => p.running,
                Series::Waiting => p.queued,
                Series::Generation => p.output,
                Series::Prompt => p.computed,
            };
            (p.time, v)
        })
        .collect();
    let tail = &values[values.len().saturating_sub(width * 2)..];
    let elapsed = tail
        .first()
        .map(|p| (now - p.0).clamp(0., 120.))
        .unwrap_or(0.);
    // Two sample columns per braille cell; left padding is unknown, never idle.
    let maximum = tail.iter().filter_map(|p| p.1).fold(1., f64::max);
    let mut cells = vec![vec![0u32; width]; 4];
    let offset = width * 2 - tail.len();
    let bits = [[1, 2, 4, 64], [8, 16, 32, 128]];
    for (index, (_, value)) in tail.iter().enumerate() {
        if let Some(v) = value {
            let x = offset + index;
            let dots = 1 + ((*v / maximum * 15.).round() as usize).min(15);
            for dy in (16 - dots)..16 {
                cells[dy / 4][x / 2] |= bits[x % 2][dy % 4];
            }
        }
    }
    let mut result: Vec<Line<'static>> = cells
        .into_iter()
        .map(|row| {
            Line::from(Span::styled(
                row.into_iter()
                    .map(|mask| {
                        if mask == 0 {
                            ' '
                        } else {
                            char::from_u32(0x2800 + mask).unwrap_or(' ')
                        }
                    })
                    .collect::<String>(),
                Style::default().fg(MAUVE),
            ))
        })
        .collect();
    let caption = match series {
        Series::Running => "running jobs",
        Series::Waiting => "waiting jobs",
        Series::Generation => "gen tok/s",
        Series::Prompt => "computed prompt tok/s · EOS",
    };
    result.push(Line::from(Span::styled(
        format!("{caption} · last {elapsed:.0}s"),
        Style::default().fg(SUB),
    )));
    result
}
fn dashboard(buf: &mut Buffer, width: u16, app: &App) -> u16 {
    let r = &app.instances[app.selected];
    let empty = Snapshot::default();
    let s = r.state.last.as_ref().map(|v| &v.snapshot).unwrap_or(&empty);
    let st = &r.state;
    let age = st.last.as_ref().map(|v| app.now - v.time);
    let status = if app.paused {
        "PAUSED".into()
    } else if let Some(e) = &st.error {
        format!("STALE · {e}")
    } else if age.is_some_and(|v| v > app.interval * 2. + 0.5) {
        "STALE · poll pending".into()
    } else if age.is_some() {
        "CONNECTED".into()
    } else {
        "CONNECTING".into()
    };
    let model = r.metadata.model.as_deref().unwrap_or("model unavailable");
    let header = vec![Line::from(format!(
        "tabbyapi-monitor  {model} @ {}  EXLLAMAV3  ● {status}  up {:.0}s  {:.1}s{}",
        r.target.url,
        app.now,
        app.interval,
        if app.mock { "  [SYNTHETIC DEMO]" } else { "" }
    ))];
    let height = panel_height(&header, width).saturating_sub(2);
    Paragraph::new(header)
        .wrap(Wrap { trim: false })
        .render(Rect::new(0, 0, width, height), buf);
    let mut y = height + 1;
    let primary_width = if width >= 80 { (width - 2) / 2 } else { width };
    let plot_width = primary_width.saturating_sub(6) as usize;
    let conc_head = vec![
        heading("CONCURRENCY"),
        Line::from(format!(
            "running {} · waiting {} · preempt — · max-seqs —",
            count(s.running()),
            count(s.queued)
        )),
    ];
    let conc = [
        conc_head,
        chart(st, app.now, plot_width, Series::Running),
        chart(st, app.now, plot_width, Series::Waiting),
    ];
    let tput_head = vec![
        heading("THROUGHPUT"),
        Line::from(format!(
            "gen {} tok/s · prompt EOS {} tok/s · tok/iter — · {} jobs/s",
            compact(st.rates.output),
            compact(st.rates.computed),
            num(st.rates.jobs)
        )),
    ];
    let tput = [
        tput_head,
        chart(st, app.now, plot_width, Series::Generation),
        chart(st, app.now, plot_width, Series::Prompt),
    ];
    let mut latency = vec![
        heading("LATENCY · engine ≈"),
        field("", format!("{:>7} {:>7} {:>7}", "p50", "p90", "p99")),
    ];
    for (name, label) in [
        ("ttft", "TTFT"),
        ("tpot", "TPOT"),
        ("total", "total"),
        ("queue", "queue"),
        ("prefill", "prefill"),
        ("decode", "decode"),
    ] {
        let h = st.histograms.get(name);
        latency.push(field(
            label,
            format!(
                "{:>7} {:>7} {:>7}",
                duration(h.and_then(|h| h.quantile(0.5))),
                duration(h.and_then(|h| h.quantile(0.9))),
                duration(h.and_then(|h| h.quantile(0.99)))
            ),
        ));
    }
    if width >= 80 {
        let a = (width - 2) / 2;
        let b = width - a - 2;
        // Pad matching sections to equal wrapped height so the charts share rows.
        let (mut left, mut right) = (vec![], vec![]);
        for (mut l, mut r) in conc.into_iter().zip(tput) {
            let rows = |lines: &[Line<'_>], w| panel_height(lines, w) - 2;
            let h = rows(&l, a).max(rows(&r, b));
            while rows(&l, a) < h {
                l.push(Line::from(""));
            }
            while rows(&r, b) < h {
                r.push(Line::from(""));
            }
            left.extend(l);
            right.extend(r);
        }
        let h = panel_height(&left, a).max(panel_height(&right, b));
        panel(buf, Rect::new(0, y, a, h), left);
        panel(buf, Rect::new(a + 2, y, b, h), right);
        y += h + 1;
    } else {
        for lines in [conc.concat(), tput.concat()] {
            let h = panel_height(&lines, width);
            panel(buf, Rect::new(0, y, width, h), lines);
            y += h + 1;
        }
    }
    let session = &st.session;
    let total_time = session.busy_seconds + session.idle_seconds;
    let mut sections = vec![vec![
        heading("CACHE & KV MEMORY"),
        field(
            "reuse",
            format!(
                "prefix hit — · sources computed {} / cached {} tok · cached share {}% (EOS)",
                compact(s.computed),
                compact(s.cached),
                num(s.cache_share())
            ),
        ),
        field(
            "memory",
            format!(
                "KV usage {}% ({}/{} tok) · reusable {} tok (overlaps used)",
                num(s.kv_percent()),
                compact(s.used),
                compact(s.capacity),
                compact(s.reusable)
            ),
        ),
    ]];
    let mut extras = vec![
        latency,
        vec![
            heading("SESSION (while serving)"),
            field(
                "average",
                format!(
                    "decode {} tok/s · prompt EOS {} tok/s · active {}% ({:.0}s busy / {:.0}s idle)",
                    compact(ratio(session.output, Some(session.busy_seconds))),
                    compact(ratio(session.computed, Some(session.busy_seconds))),
                    num(ratio(Some(session.busy_seconds * 100.), Some(total_time))),
                    session.busy_seconds,
                    session.idle_seconds
                ),
            ),
            field(
                "totals",
                format!(
                    "{} jobs · {} gen · {} computed / {} cached prompt tokens · {:.0}s unknown",
                    count(session.completed),
                    compact(session.output),
                    compact(session.computed),
                    compact(session.cached),
                    session.unknown_seconds
                ),
            ),
        ],
    ];
    // Compact inline sections, as in vllmstat; wholly unsupported panels are hidden.
    extras.push(vec![inline(
        "OUTCOMES",
        format!(
            "{} completed · {} cancelled · {} error · {} observation errors (lifetime jobs)",
            count(s.completed),
            count(s.cancelled),
            count(s.errors),
            count(s.observation_errors)
        ),
    )]);
    if s.accepted.is_some() || s.rejected.is_some() {
        extras.push(vec![inline(
            "SPEC DECODE",
            format!(
                "acceptance {}% · {} accepted / {} rejected draft tokens (EOS)",
                num(s.mtp_percent()),
                compact(s.accepted),
                compact(s.rejected)
            ),
        )]);
    }
    if s.queued.is_some_and(|v| v > 0.) || s.kv_percent().is_some_and(|v| v > 95.) {
        extras.push(vec![
            heading("CONFIG ADVISOR"),
            Line::from(if s.queued.is_some_and(|v| v > 0.) {
                "Queued engine jobs observed; watch concurrency and latency."
            } else {
                "KV occupancy exceeds 95%; watch memory pressure."
            }),
        ]);
    }
    if app.show_gpu {
        let mut lines = vec![];
        for g in &app.gpu {
            if !r.target.gpus.is_empty() && !r.target.gpus.contains(&g.index) {
                continue;
            }
            lines.push(Line::from(format!(
                "GPU {:<2} {:<24.24} {:>5}%  {:>11} GiB  {:>4}°C  {:>9} W  fan {:>3}%  clk {:>11} MHz · {}",
                g.index,
                g.name,
                num(g.util_percent),
                format!(
                    "{}/{}",
                    num(g.used_mib.map(|v| v / 1024.)),
                    num(g.total_mib.map(|v| v / 1024.))
                ),
                num(g.temp_c),
                format!("{}/{}", count(g.power_w), count(g.power_limit_w)),
                count(g.fan_percent),
                format!("{}/{}", count(g.sm_mhz), count(g.memory_mhz)),
                app.gpu_source
            )));
        }
        if lines.is_empty() {
            lines.push(Line::from(format!(
                "GPU  — {} ({})",
                app.gpu_error.as_deref().unwrap_or("provider pending"),
                app.gpu_source
            )));
        } else if let Some(error) = &app.gpu_error {
            lines.push(Line::from(Span::styled(
                format!("STALE: {error}"),
                Style::default().fg(RED),
            )));
        }
        sections.push(lines);
    }
    if app.show_tee {
        sections.push(vec![
            heading("TEE"),
            Line::from("Request feed unavailable · TabbyAPI metrics do not expose request text."),
            Line::from(""),
            Line::from(""),
            Line::from(""),
            Line::from(""),
        ]);
    }
    if app.expanded {
        sections.extend(extras);
    }
    for lines in sections {
        let h = panel_height(&lines, width);
        panel(buf, Rect::new(0, y, width, h), lines);
        y += h + 1;
    }
    y.saturating_sub(1)
}
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(
        Block::default().style(Style::default().bg(BASE).fg(TEXT)),
        area,
    );
    if area.width < 30 || area.height < 8 {
        f.render_widget(
            Paragraph::new("Terminal too small (min 30×8); q quits")
                .style(Style::default().fg(MAUVE)),
            area,
        );
        return;
    }
    let view_h = area.height.saturating_sub(2);
    if app.detail {
        let mut buffer = Buffer::empty(Rect::new(0, 0, area.width, 240));
        buffer.set_style(buffer.area, Style::default().fg(TEXT).bg(BASE));
        let height = dashboard(&mut buffer, area.width, app);
        app.scroll = app.scroll.min(height.saturating_sub(view_h));
        let target = f.buffer_mut();
        for y in 0..view_h.min(height) {
            for x in 0..area.width {
                target[(area.x + x, area.y + y)] = buffer[(x, y + app.scroll)].clone();
            }
        }
    } else {
        let mut lines = vec![
            Line::from(Span::styled(
                format!(
                    "TABBYAPI FLEET · {} targets{}",
                    app.instances.len(),
                    if app.mock { " · SYNTHETIC DEMO" } else { "" }
                ),
                Style::default().fg(MAUVE).add_modifier(Modifier::BOLD),
            )),
            Line::from(format!(
                "  {:<28} {:<9} {:>4} {:>6} {:>9} {:>5} {:>6}",
                "INSTANCE", "STATE", "RUN", "QUEUE", "GEN t/s", "KV%", "TTFT≈"
            )),
        ];
        let capacity = view_h.saturating_sub(4) as usize;
        let offset = app.selected.saturating_sub(capacity.saturating_sub(1));
        for (i, r) in app.instances.iter().enumerate().skip(offset).take(capacity) {
            let empty = Snapshot::default();
            let s = r.state.last.as_ref().map(|v| &v.snapshot).unwrap_or(&empty);
            let stale = r.state.error.is_some()
                || r.state
                    .last
                    .as_ref()
                    .is_some_and(|v| app.now - v.time > app.interval * 2. + 0.5);
            let status = if stale {
                "STALE"
            } else if r.state.last.is_some() {
                "UP"
            } else {
                "WAIT"
            };
            let mut text = format!(
                "{} {:<28.28} {:<9} {:>4} {:>6} {:>9} {:>5} {:>6}",
                if i == app.selected { "▸" } else { " " },
                r.target.name,
                status,
                count(s.running()),
                count(s.queued),
                num(r.state.rates.output),
                num(s.kv_percent()),
                duration(r.state.histograms.get("ttft").and_then(|h| h.quantile(0.5)))
            );
            if app.show_gpu {
                text.push_str(&format!(
                    " GPU {}",
                    app.gpu
                        .iter()
                        .find(|g| r.target.gpus.contains(&g.index))
                        .map(|g| format!("{}%", num(g.util_percent)))
                        .unwrap_or_else(|| "—".into())
                ));
            }
            lines.push(Line::from(Span::styled(
                text,
                Style::default()
                    .fg(if i == app.selected {
                        MAUVE
                    } else if stale {
                        RED
                    } else {
                        GREEN
                    })
                    .bg(if i == app.selected { SURFACE } else { BASE }),
            )));
        }
        lines.push(Line::from("↑/↓ j/k select · Enter dashboard · Esc fleet"));
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(MAUVE)),
            ),
            Rect::new(area.x, area.y, area.width, view_h),
        );
    }
    let footer = vec![
        Line::from(Span::styled(
            format!(
                "q quit · p {} · g GPU · r reset · t feed · d details · +/- {:.1}s · j/k scroll",
                if app.paused {
                    "RESUME [PAUSED]"
                } else {
                    "pause"
                },
                app.interval
            ),
            Style::default().fg(MAUVE),
        )),
        Line::from(Span::styled(
            match (app.status.is_empty(), app.detail) {
                (_, false) => app.status.clone(),
                (true, true) => "PgUp/PgDn/Home · Esc fleet".into(),
                (false, true) => format!("{} · PgUp/PgDn/Home · Esc fleet", app.status),
            },
            Style::default().fg(SUB),
        )),
    ];
    f.render_widget(
        Paragraph::new(footer).style(Style::default().bg(SURFACE)),
        Rect::new(area.x, area.y + view_h, area.width, 2),
    );
}
pub fn plain(r: &Runtime) -> String {
    let empty = Snapshot::default();
    let s = r.state.last.as_ref().map(|v| &v.snapshot).unwrap_or(&empty);
    format!(
        "TabbyAPI · {}\nModel {} · health {}\nCONCURRENCY  {} running / {} queued engine jobs\nTHROUGHPUT   — requires two samples\nCACHE & KV   {}% · {} / {} tokens; {} reusable\nOUTCOMES     {} completed / {} cancelled / {} error\nMTP          {}% draft acceptance\nSESSION      starts with interactive polling\n{}",
        r.target.name,
        r.metadata.model.as_deref().unwrap_or("—"),
        r.metadata.health.as_deref().unwrap_or("—"),
        count(s.running()),
        count(s.queued),
        num(s.kv_percent()),
        count(s.used),
        count(s.capacity),
        count(s.reusable),
        count(s.completed),
        count(s.cancelled),
        count(s.errors),
        num(s.mtp_percent()),
        r.state
            .error
            .as_deref()
            .unwrap_or("Read-only schema-v1 snapshot; engine timings, not client timings.")
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Cli, Config};
    use clap::Parser;
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::Instant;
    #[test]
    fn theme_and_resize() {
        let c = Config::from_cli(Cli::parse_from(["m", "--mock"])).unwrap();
        let mut app = App::new(&c, Instant::now());
        for (w, h) in [(120, 40), (70, 24), (30, 8), (15, 4)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let b = terminal.backend().buffer();
            assert_eq!(b[(0, 0)].bg, BASE);
            assert!(b.content.iter().any(|c| c.fg == MAUVE));
        }
    }
    #[test]
    fn reference_dashboard_has_embedded_four_row_charts() {
        let c = Config::from_cli(Cli::parse_from(["m", "--mock"])).unwrap();
        let mut app = App::new(&c, Instant::now());
        for t in 0..30 {
            app.instances[0]
                .state
                .update(t as f64, crate::poll::mock(t as f64).snapshot.unwrap());
        }
        app.now = 30.;
        let mut terminal = Terminal::new(TestBackend::new(150, 54)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let b = terminal.backend().buffer();
        for (start, end) in [(1, 73), (77, 149)] {
            let rows = (4..20)
                .filter(|y| {
                    (start..end).any(|x| {
                        b[(x, *y)]
                            .symbol()
                            .chars()
                            .any(|c| (0x2801..=0x28ff).contains(&(c as u32)))
                    })
                })
                .count();
            assert!(
                rows >= 4,
                "reference chart missing from primary panel: {rows} braille rows"
            );
        }
        let text = b.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(!text.contains("THROUGHPUT HISTORY"));
        assert!(!text.contains("REQUEST SHAPE / EFFICIENCY"));
        for absent in [
            "LATENCY",
            "SESSION",
            "OUTCOMES",
            "SPEC DECODE",
            "CONFIG ADVISOR",
        ] {
            assert!(!text.contains(absent), "extra default category: {absent}");
        }
        assert!(text.contains("TEE"));
        assert_ne!(b[(0, 0)].symbol(), "╭");
        assert!(text.contains("CACHE & KV MEMORY"));
        let concurrency = (0..54)
            .find(|y| {
                (0..150)
                    .map(|x| b[(x, *y)].symbol())
                    .collect::<String>()
                    .contains("CONCURRENCY")
            })
            .unwrap();
        let row = (0..150)
            .map(|x| b[(x, concurrency)].symbol())
            .collect::<String>();
        assert!(row.contains("THROUGHPUT"));
        assert_eq!(b[(74, concurrency)].symbol(), " ");
        app.key(crossterm::event::KeyCode::Char('d'));
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let expanded = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(expanded.contains("LATENCY"));
        assert!(expanded.contains("SESSION"));
    }
    fn bottom(lines: &[Line<'_>]) -> String {
        lines[3].spans.iter().map(|s| s.content.as_ref()).collect()
    }
    fn fed(fail_at: &[f64]) -> State {
        let mut st = State::default();
        for t in 0..40 {
            let t = t as f64;
            if fail_at.contains(&t) {
                st.fail(t, "timeout".into());
            } else {
                st.update(t, crate::poll::mock(t).snapshot.unwrap());
            }
        }
        st
    }
    #[test]
    fn charts_are_continuous_and_share_columns() {
        let st = fed(&[17.]);
        let rows: Vec<String> = [
            Series::Running,
            Series::Waiting,
            Series::Generation,
            Series::Prompt,
        ]
        .into_iter()
        .map(|series| bottom(&chart(&st, 39., 30, series)))
        .collect();
        let first = rows[0].find(|c| c != ' ').unwrap();
        for row in &rows {
            assert_eq!(
                row.find(|c| c != ' '),
                Some(first),
                "chart start differs: {rows:?}"
            );
            let line = &row[first..];
            assert!(!line.contains(' '), "gap after one failed poll: {line:?}");
        }
        let outage = fed(&[16., 17.]);
        assert!(
            bottom(&chart(&outage, 39., 30, Series::Generation))
                .trim()
                .contains(' ')
        );
    }
    #[test]
    fn labelled_rows_share_value_columns() {
        let c = Config::from_cli(Cli::parse_from([
            "m",
            "--mock",
            "-u",
            "http://a:1",
            "-u",
            "http://b:2",
        ]))
        .unwrap();
        let mut app = App::new(&c, Instant::now());
        app.expanded = true;
        app.instances[0].state = fed(&[]);
        app.now = 39.;
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 240));
        dashboard(&mut buf, 120, &app);
        let text: Vec<String> = (0..240)
            .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let start = |label: &str| {
            let row = text
                .iter()
                .find(|r| r.contains(&format!("│ {label} ")))
                .unwrap();
            let at = row.find(label).unwrap() + label.len();
            at + row[at..].find(|c| c != ' ').unwrap()
        };
        for group in [
            &["reuse", "memory", "average", "totals"][..],
            &["TTFT", "TPOT", "total", "queue", "prefill", "decode"][..],
            &["OUTCOMES", "SPEC DECODE"][..],
        ] {
            let columns: Vec<_> = group.iter().map(|l| start(l)).collect();
            assert!(
                columns.windows(2).all(|w| w[0] == w[1]),
                "{group:?} → {columns:?}"
            );
        }
        let mut terminal = Terminal::new(TestBackend::new(110, 20)).unwrap();
        app.detail = false;
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let b = terminal.backend().buffer();
        // One ASCII byte per cell, so byte offsets are terminal columns.
        let row = |y: u16| {
            (0..110)
                .map(|x| {
                    b[(x, y)]
                        .symbol()
                        .chars()
                        .next()
                        .filter(char::is_ascii)
                        .unwrap_or('#')
                })
                .collect::<String>()
        };
        let (header, first) = (row(2), row(3));
        for (h, v) in [("STATE", "UP"), ("RUN", "1"), ("QUEUE", "0")] {
            let end = header.find(h).unwrap() + h.len();
            let cell = if h == "STATE" {
                first.find(v).unwrap()
            } else {
                first[..end].rfind(v).unwrap() + 1
            };
            assert_eq!(
                cell,
                if h == "STATE" {
                    header.find(h).unwrap()
                } else {
                    end
                },
                "{h}"
            );
        }
    }
}

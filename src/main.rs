mod config;
mod metrics;
mod poll;
mod ui;
use clap::Parser;
use config::{Cli, Config, Target};
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use metrics::State;
use poll::{Gpu, GpuWorker, Metadata, Worker};
use ratatui::{Terminal, backend::CrosstermBackend};
use serde_json::{Value, json};
use std::{
    io::{self, IsTerminal},
    time::{Duration, Instant},
};

pub struct Runtime {
    pub target: Target,
    pub metadata: Metadata,
    pub state: State,
    worker: Worker,
    next_poll: f64,
    last_meta: f64,
}
pub struct App {
    origin: Instant,
    pub instances: Vec<Runtime>,
    pub selected: usize,
    pub detail: bool,
    pub paused: bool,
    pub show_gpu: bool,
    pub show_tee: bool,
    pub expanded: bool,
    pub interval: f64,
    pub scroll: u16,
    pub gpu: Vec<Gpu>,
    pub gpu_source: String,
    pub gpu_error: Option<String>,
    pub gpu_last: Option<f64>,
    pub mock: bool,
    pub now: f64,
    pub status: String,
}
impl App {
    fn new(c: &Config, start: Instant) -> Self {
        let instances = c
            .targets
            .iter()
            .map(|t| Runtime {
                target: t.clone(),
                metadata: Metadata::default(),
                state: State::default(),
                worker: Worker::new(t.clone(), c.path.clone(), c.timeout, c.mock, start),
                next_poll: 0.,
                last_meta: -60.,
            })
            .collect();
        Self {
            origin: start,
            instances,
            selected: 0,
            detail: c.targets.len() == 1,
            paused: false,
            show_gpu: c.gpu != "none",
            show_tee: true,
            expanded: false,
            interval: c.interval,
            scroll: 0,
            gpu: vec![],
            gpu_source: if c.gpu == "ssh" {
                format!("ssh:{}", c.gpu_host)
            } else {
                c.gpu.clone()
            },
            gpu_error: None,
            gpu_last: None,
            mock: c.mock,
            now: 0.,
            status: String::new(),
        }
    }
    fn key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('q') => return true,
            KeyCode::Char('p') => {
                self.paused = !self.paused;
                self.status = if self.paused {
                    "Polling paused"
                } else {
                    "Polling resumed"
                }
                .into();
                for r in &mut self.instances {
                    r.state.gap = true;
                    r.next_poll = self.now;
                }
            }
            KeyCode::Char('g') => {
                self.show_gpu = !self.show_gpu;
                self.status = if self.show_gpu {
                    "GPU shown"
                } else {
                    "GPU hidden"
                }
                .into();
            }
            KeyCode::Char('d') => {
                self.expanded = !self.expanded;
                self.scroll = 0;
                self.status = if self.expanded {
                    "Details shown"
                } else {
                    "Details hidden"
                }
                .into();
            }
            KeyCode::Char('t') => {
                self.show_tee = !self.show_tee;
                self.status = if self.show_tee {
                    "Feed shown"
                } else {
                    "Feed hidden"
                }
                .into();
            }
            KeyCode::Char('r') => {
                self.instances[self.selected].state.reset();
                self.instances[self.selected].next_poll = self.now;
                self.status = "Session reset".into();
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.interval = (self.interval / 2.).max(0.1);
                self.status = format!("Interval {:.1}s", self.interval);
            }
            KeyCode::Char('-') => {
                self.interval = (self.interval * 2.).min(60.);
                self.status = format!("Interval {:.1}s", self.interval);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.detail {
                    self.scroll = self.scroll.saturating_add(3)
                } else {
                    self.selected = (self.selected + 1) % self.instances.len();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.detail {
                    self.scroll = self.scroll.saturating_sub(3)
                } else {
                    self.selected =
                        (self.selected + self.instances.len() - 1) % self.instances.len();
                }
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(12),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(12),
            KeyCode::Home => self.scroll = 0,
            KeyCode::Enter => {
                self.detail = true;
                self.scroll = 0;
            }
            KeyCode::Esc if self.instances.len() > 1 => {
                self.detail = false;
                self.scroll = 0;
            }
            _ => {}
        }
        false
    }
    fn receive(&mut self) -> bool {
        let mut changed = false;
        for r in &mut self.instances {
            if let Ok(reply) = r.worker.rx.try_recv() {
                changed = true;
                r.worker.busy = false;
                if let Some(meta) = reply.metadata {
                    r.metadata = meta;
                    r.last_meta = if r.metadata.model.is_some() {
                        self.now
                    } else {
                        self.now - 59.
                    };
                }
                match reply.snapshot {
                    Ok(s) => r.state.update(
                        reply.collected_at.duration_since(self.origin).as_secs_f64(),
                        s,
                    ),
                    Err(e) => r.state.fail(self.now, e),
                };
                r.next_poll = self.now + self.interval;
            }
        }
        changed
    }
    fn start_polls(&mut self) {
        if self.paused {
            return;
        }
        for r in &mut self.instances {
            if self.now >= r.next_poll && !r.worker.busy {
                r.worker.start(self.now - r.last_meta >= 60.);
            }
        }
    }
}
fn snapshot_json(r: &Runtime, now: f64, mock: bool) -> Value {
    let s = r.state.last.as_ref().map(|v| &v.snapshot);
    let latency: std::collections::BTreeMap<_, _> = r
        .state
        .histograms
        .iter()
        .map(|(n, h)| {
            (
                n,
                json!({"p50":h.quantile(0.5),"p90":h.quantile(0.9),"p99":h.quantile(0.99)}),
            )
        })
        .collect();
    let lifetime: std::collections::BTreeMap<_, _> = s
        .map(|s| {
            s.histograms
                .iter()
                .map(|(n, h)| {
                    (
                        n,
                        json!({"p50":h.quantile(0.5),"p90":h.quantile(0.9),"p99":h.quantile(0.99)}),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    json!({"schema_version":1,"engine_latency_lifetime_approx_seconds":lifetime,"name":r.target.name,"endpoint":r.target.url,"mock":mock,"metadata":r.metadata,"connected":r.state.error.is_none()&&s.is_some(),"error":r.state.error,"sample_age_seconds":r.state.last.as_ref().map(|v|(now-v.time).max(0.)),"snapshot":s,"rates":r.state.rates,"session":r.state.session,"latency_window_seconds":120,"engine_latency_approx_seconds":latency,"metric_notes":{"prompt":"successful EOS only","requests":"engine jobs","ttft":"engine admission to first output; not client wire latency","missing":"null; never substituted with zero"}})
}
struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
    }
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tabbyapi-monitor: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
fn run() -> Result<(), String> {
    let c = Config::from_cli(Cli::parse())?;
    if c.once || !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        let start = Instant::now();
        let mut app = App::new(&c, start);
        app.start_polls();
        while app.instances.iter().any(|r| r.worker.busy) {
            app.now = start.elapsed().as_secs_f64();
            app.receive();
            std::thread::sleep(Duration::from_millis(5));
        }
        if c.gpu != "none" {
            match poll::gpu(&c.gpu, &c.gpu_host, c.timeout) {
                Ok(g) => app.gpu = g,
                Err(e) => app.gpu_error = Some(e),
            }
        }
        let values: Vec<_> = app
            .instances
            .iter()
            .map(|r| {
                let mut value = snapshot_json(r, app.now, c.mock);
                value["gpu_source"] = json!(app.gpu_source);
                value["host_gpus"] = json!(
                    app.gpu
                        .iter()
                        .filter(|g| r.target.gpus.is_empty() || r.target.gpus.contains(&g.index))
                        .collect::<Vec<_>>()
                );
                value["gpu_error"] = json!(app.gpu_error);
                value
            })
            .collect();
        if c.json {
            let output = if values.len() == 1 {
                values[0].clone()
            } else {
                Value::Array(values)
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&output).map_err(|_| "cannot serialize snapshot")?
            );
        } else {
            for r in &app.instances {
                println!("{}", ui::plain(r));
                if c.gpu != "none" {
                    println!("GPU source {}", app.gpu_source);
                    for g in &app.gpu {
                        println!(
                            "GPU {} {}: {}% · {} / {} MiB · {} W",
                            g.index,
                            g.name,
                            ui::num(g.util_percent),
                            ui::num(g.used_mib),
                            ui::num(g.total_mib),
                            ui::num(g.power_w)
                        );
                    }
                    if let Some(e) = &app.gpu_error {
                        println!("GPU — {e}");
                    }
                }
            }
        }
        return if app.instances.iter().any(|r| r.state.error.is_some()) {
            Err("one or more metrics targets failed".into())
        } else {
            Ok(())
        };
    }
    let start = Instant::now();
    let mut app = App::new(&c, start);
    let mut gpu_worker = GpuWorker::new(c.gpu.clone(), c.gpu_host.clone(), c.timeout);
    let mut next_gpu = 0.;
    terminal::enable_raw_mode().map_err(|_| "cannot enable terminal input")?;
    let _restore = Restore;
    execute!(io::stdout(), EnterAlternateScreen, Hide).map_err(|_| "cannot enter terminal")?;
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        old_hook(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|_| "cannot initialize terminal")?;
    let stopping = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(sig, stopping.clone())
            .map_err(|_| "cannot register termination handler")?;
    }
    let mut dirty = true;
    let mut last_draw = 0.;
    loop {
        if stopping.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        app.now = start.elapsed().as_secs_f64();
        dirty |= app.receive();
        app.start_polls();
        if let Ok(result) = gpu_worker.rx.try_recv() {
            dirty = true;
            gpu_worker.busy = false;
            match result {
                Ok(g) => {
                    app.gpu = g;
                    app.gpu_error = None;
                    app.gpu_last = Some(app.now);
                }
                Err(e) => {
                    app.gpu_error = Some(e);
                }
            }
            next_gpu = app.now + app.interval.max(1.);
        }
        if !app.paused && app.show_gpu && !gpu_worker.busy && app.now >= next_gpu {
            gpu_worker.start();
        }
        if dirty || app.now - last_draw >= 1. {
            terminal
                .draw(|f| ui::draw(f, &mut app))
                .map_err(|_| "terminal draw failed")?;
            dirty = false;
            last_draw = app.now;
        }
        if event::poll(Duration::from_millis(100)).map_err(|_| "terminal input failed")? {
            match event::read().map_err(|_| "terminal input failed")? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    dirty = true;
                    if (k.code == KeyCode::Char('c')
                        && k.modifiers.contains(event::KeyModifiers::CONTROL))
                        || app.key(k.code)
                    {
                        break;
                    }
                }
                Event::Resize(_, _) => {
                    dirty = true;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls() {
        let c = Config::from_cli(Cli::parse_from([
            "m", "--mock", "--url", "http://a", "--url", "http://b",
        ]))
        .unwrap();
        let mut app = App::new(&c, Instant::now());
        assert!(!app.detail);
        app.key(KeyCode::Down);
        assert_eq!(app.selected, 1);
        app.key(KeyCode::Enter);
        assert!(app.detail);
        app.key(KeyCode::Esc);
        assert!(!app.detail);
        app.key(KeyCode::Char('p'));
        assert!(app.paused);
        app.key(KeyCode::Char('+'));
        assert_eq!(app.interval, 0.5);
        app.key(KeyCode::Char('-'));
        assert_eq!(app.interval, 1.);
        app.key(KeyCode::Char('r'));
        assert_eq!(app.status, "Session reset");
        assert!(app.key(KeyCode::Char('q')));
    }
}

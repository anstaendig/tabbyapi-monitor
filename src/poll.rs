use crate::{
    config::{Target, safe_text},
    metrics::{self, Snapshot},
};
use serde::Serialize;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

pub const BODY_LIMIT: u64 = 1024 * 1024;
#[derive(Clone, Default, Serialize)]
pub struct Metadata {
    pub model: Option<String>,
    pub context: Option<u64>,
    pub health: Option<String>,
}
#[derive(Clone, Default, Serialize)]
pub struct Gpu {
    pub index: usize,
    pub name: String,
    pub used_mib: Option<f64>,
    pub total_mib: Option<f64>,
    pub util_percent: Option<f64>,
    pub temp_c: Option<f64>,
    pub power_w: Option<f64>,
    pub power_limit_w: Option<f64>,
    pub sm_mhz: Option<f64>,
    pub memory_mhz: Option<f64>,
    pub fan_percent: Option<f64>,
}
pub struct Reply {
    pub collected_at: Instant,
    pub snapshot: Result<Snapshot, String>,
    pub metadata: Option<Metadata>,
}
fn get(agent: &ureq::Agent, target: &Target, path: &str) -> Result<String, String> {
    let url = format!("{}{}", target.url, path);
    let mut request = agent
        .get(&url)
        .header("Accept", "text/plain, application/json");
    if let Some(key) = &target.api_key {
        request = request.header("Authorization", format!("Bearer {key}"));
    }
    let mut response = request.call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        _ => "request failed or timed out".into(),
    })?;
    let mut body = String::new();
    response
        .body_mut()
        .as_reader()
        .take(BODY_LIMIT + 1)
        .read_to_string(&mut body)
        .map_err(|_| "invalid or incomplete HTTP body")?;
    if body.len() as u64 > BODY_LIMIT {
        return Err("HTTP body exceeds 1 MiB limit".into());
    }
    Ok(body)
}
fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .build()
        .into()
}
pub fn collect(target: &Target, path: &str, timeout: Duration, metadata: bool) -> Reply {
    let agent = agent(timeout);
    let snapshot = get(&agent, target, path).and_then(|t| metrics::parse(&t));
    let collected_at = Instant::now();
    let metadata = if metadata && snapshot.is_ok() {
        let mut m = Metadata::default();
        if let Ok(text) = get(&agent, target, "/v1/model") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                m.model = v["id"].as_str().map(safe_text);
                m.context = v["parameters"]["max_seq_len"].as_u64();
            }
        }
        if let Ok(text) = get(&agent, target, "/health") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                m.health = v["status"].as_str().map(safe_text);
            }
        }
        Some(m)
    } else {
        None
    };
    Reply {
        snapshot,
        metadata,
        collected_at,
    }
}
pub fn mock(t: f64) -> Reply {
    let text = include_str!("../tests/fixtures/metrics.prom");
    let mut s = metrics::parse(text).expect("compiled mock fixture must be valid");
    let work = t * 72.;
    s.output = Some(23850. + work);
    s.computed = Some(2_213_424. + (t / 5.).floor() * 2048.);
    s.cached = Some((t / 5.).floor() * 1024.);
    s.completed = Some(84. + (t / 5.).floor());
    s.decode = Some(1.);
    s.queued = Some(0.);
    s.used = Some(32000. + (t.sin() + 1.) * 8000.);
    Reply {
        collected_at: Instant::now(),
        snapshot: Ok(s),
        metadata: Some(Metadata {
            model: Some("DEMO · synthetic ExLlamaV3".into()),
            context: Some(262144),
            health: Some("mock".into()),
        }),
    }
}
pub struct Worker {
    tx: SyncSender<bool>,
    pub rx: Receiver<Reply>,
    pub busy: bool,
}
impl Worker {
    pub fn new(
        target: Target,
        path: String,
        timeout: Duration,
        mock_mode: bool,
        start: Instant,
    ) -> Self {
        let (tx, commands) = mpsc::sync_channel(1);
        let (results, rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("metrics-poll".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                while let Ok(meta) = commands.recv() {
                    let result = if mock_mode {
                        mock(start.elapsed().as_secs_f64())
                    } else {
                        collect(&target, &path, timeout, meta)
                    };
                    if results.send(result).is_err() {
                        break;
                    }
                }
            })
            .expect("poll thread");
        Self {
            tx,
            rx,
            busy: false,
        }
    }
    pub fn start(&mut self, metadata: bool) {
        if !self.busy && self.tx.try_send(metadata).is_ok() {
            self.busy = true;
        }
    }
}
pub fn gpu(mode: &str, host: &str, timeout: Duration) -> Result<Vec<Gpu>, String> {
    if mode == "none" {
        return Ok(vec![]);
    }
    let mut command = if mode == "ssh" {
        let mut c = Command::new("ssh");
        c.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=2",
            "--",
            host,
            "nvidia-smi",
        ]);
        c
    } else {
        Command::new("nvidia-smi")
    };
    command.args(["--query-gpu=index,name,memory.used,memory.total,utilization.gpu,temperature.gpu,power.draw,power.limit,clocks.sm,clocks.mem,fan.speed","--format=csv,noheader,nounits"]);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "GPU provider unavailable")?;
    let out = child.stdout.take().ok_or("GPU output unavailable")?;
    // Drain a bounded output buffer concurrently so a full pipe cannot hang wait.
    let reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = out.take(65537).read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => break None,
        };
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let text = reader.join().unwrap_or_default();
    if !status.is_some_and(|s| s.success()) || text.len() > 65536 {
        return Err("GPU query failed or timed out".into());
    }
    let mut gpus = vec![];
    for line in text.lines().take(32) {
        let p: Vec<_> = line.split(',').map(str::trim).collect();
        if p.len() != 11 {
            continue;
        }
        let num = |i: usize| {
            p[i].parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.)
        };
        if let Ok(index) = p[0].parse() {
            gpus.push(Gpu {
                index,
                name: safe_text(p[1]),
                used_mib: num(2),
                total_mib: num(3),
                util_percent: num(4),
                temp_c: num(5),
                power_w: num(6),
                power_limit_w: num(7),
                sm_mhz: num(8),
                memory_mhz: num(9),
                fan_percent: num(10),
            });
        }
    }
    if gpus.is_empty() {
        Err("GPU metrics unavailable".into())
    } else {
        Ok(gpus)
    }
}
pub struct GpuWorker {
    tx: SyncSender<()>,
    pub rx: Receiver<Result<Vec<Gpu>, String>>,
    pub busy: bool,
}
impl GpuWorker {
    pub fn new(mode: String, host: String, timeout: Duration) -> Self {
        let (tx, commands) = mpsc::sync_channel(1);
        let (results, rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            while commands.recv().is_ok() {
                if results.send(gpu(&mode, &host, timeout)).is_err() {
                    break;
                }
            }
        });
        Self {
            tx,
            rx,
            busy: false,
        }
    }
    pub fn start(&mut self) {
        if !self.busy && self.tx.try_send(()).is_ok() {
            self.busy = true;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    fn serve(body: &str, status: &str) -> Target {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 2048];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(response.as_bytes());
        });
        Target {
            url,
            name: String::new(),
            api_key: None,
            gpus: vec![],
        }
    }
    #[test]
    fn http_parser() {
        let t = serve(include_str!("../tests/fixtures/metrics.prom"), "200 OK");
        assert!(
            collect(&t, "/metrics", Duration::from_secs(2), false)
                .snapshot
                .is_ok()
        );
    }
    #[test]
    fn auth_error() {
        let t = serve("secret error body", "401 Unauthorized");
        let e = collect(&t, "/metrics", Duration::from_secs(2), false)
            .snapshot
            .unwrap_err();
        assert_eq!(e, "HTTP 401");
        assert!(!e.contains("secret"));
    }
    #[test]
    fn bounded_body() {
        let t = serve(&"x".repeat(BODY_LIMIT as usize + 10), "200 OK");
        assert!(
            collect(&t, "/metrics", Duration::from_secs(2), false)
                .snapshot
                .unwrap_err()
                .contains("1 MiB")
        );
    }
}

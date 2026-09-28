use clap::Parser;
use serde::Deserialize;
use std::io::Read;
use std::{collections::HashSet, path::PathBuf, time::Duration};

#[derive(Parser, Debug)]
#[command(version, about = "Terminal dashboard for TabbyAPI + ExLlamaV3 servers")]
pub struct Cli {
    /// TabbyAPI base URL; repeat for a fleet [default: http://localhost:8080]
    #[arg(short = 'u', long = "url", alias = "endpoint", value_name = "URL")]
    pub urls: Vec<String>,
    /// TOML fleet config, or a TabbyAPI config.yml to read host/port from
    #[arg(long, env = "TABBYAPI_MONITOR_CONFIG")]
    pub config: Option<PathBuf>,
    /// Refresh interval in seconds, 0.1-60 [default: 1]
    #[arg(short = 'i', long)]
    pub interval: Option<f64>,
    /// Per-request and GPU-query timeout in seconds, 0.05-60
    #[arg(long, default_value = "3")]
    pub timeout: f64,
    /// Path of the Prometheus metrics endpoint
    #[arg(long, default_value = "/metrics")]
    pub metrics_path: String,
    /// Bearer API key (prefer the environment variable over the flag)
    #[arg(long, env = "TABBYAPI_API_KEY", hide_env_values = true)]
    pub api_key: Option<String>,
    /// Disable GPU readings (same as --gpu none)
    #[arg(long)]
    pub no_gpu: bool,
    /// GPU readings via nvidia-smi: locally, over SSH, or none [default: local]
    #[arg(long,value_parser=["none","local","ssh"])]
    pub gpu: Option<String>,
    /// SSH host for `--gpu ssh`
    #[arg(long)]
    pub gpu_host: Option<String>,
    /// Synthetic demo data; no server is contacted
    #[arg(long)]
    pub mock: bool,
    /// Print one snapshot and exit
    #[arg(long)]
    pub once: bool,
    /// With --once, print the snapshot as JSON
    #[arg(long, requires = "once")]
    pub json: bool,
}
#[derive(Clone, Deserialize, Debug)]
pub struct Target {
    pub url: String,
    #[serde(default)]
    pub name: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub gpus: Vec<usize>,
}
#[derive(Default, Deserialize)]
struct FileConfig {
    interval: Option<f64>,
    gpu: Option<bool>,
    #[serde(default, alias = "instances")]
    instance: Vec<Target>,
}
pub struct Config {
    pub targets: Vec<Target>,
    pub interval: f64,
    pub timeout: Duration,
    pub path: String,
    pub gpu: String,
    pub gpu_host: String,
    pub mock: bool,
    pub once: bool,
    pub json: bool,
}
pub fn safe_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(160).collect()
}
pub fn normalize_url(input: &str) -> Result<String, String> {
    let input = input.trim().trim_end_matches('/');
    let uri: ureq::http::Uri = input.parse().map_err(|_| "invalid endpoint URL")?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none() {
        return Err("endpoint must be an absolute HTTP(S) URL".into());
    }
    if uri.authority().unwrap().as_str().contains('@')
        || input.contains('#')
        || uri.query().is_some()
    {
        return Err(
            "endpoint must not contain credentials, query, or fragment; use --api-key".into(),
        );
    }
    Ok(input.to_owned())
}
fn yaml_url(text: &str) -> Result<String, String> {
    let (mut host, mut port, mut network) = ("127.0.0.1".to_owned(), "8080".to_owned(), false);
    for line in text.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            network = s == "network:";
            continue;
        }
        if network {
            if let Some((k, v)) = s.split_once(':') {
                let v = v
                    .split('#')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches(['\'', '"']);
                match k {
                    "host" => host = v.into(),
                    "port" => port = v.into(),
                    _ => {}
                }
            }
        }
    }
    port.parse::<u16>()
        .map_err(|_| "invalid network port in YAML")?;
    if host == "0.0.0.0" {
        host = "127.0.0.1".into();
    }
    if host == "::" {
        host = "::1".into();
    }
    if host.contains(':') && !host.starts_with('[') {
        host = format!("[{host}]");
    }
    normalize_url(&format!("http://{host}:{port}"))
}
impl Config {
    pub fn from_cli(cli: Cli) -> Result<Self, String> {
        let mut file = FileConfig::default();
        let config_path = cli.config.or_else(|| {
            let local = PathBuf::from("tabbyapi-monitor.toml");
            if local.is_file() {
                Some(local)
            } else {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(".config/tabbyapi-monitor/config.toml"))
                    .filter(|p| p.is_file())
            }
        });
        if let Some(path) = config_path {
            let mut text = String::new();
            std::fs::File::open(&path)
                .map_err(|_| "cannot read config file")?
                .take(1024 * 1024 + 1)
                .read_to_string(&mut text)
                .map_err(|_| "cannot read config file")?;
            if text.len() > 1024 * 1024 {
                return Err("configuration exceeds 1 MiB".into());
            }
            if matches!(
                path.extension().and_then(|p| p.to_str()),
                Some("yaml" | "yml")
            ) {
                file.instance.push(Target {
                    url: yaml_url(&text)?,
                    name: String::new(),
                    api_key: None,
                    gpus: vec![],
                });
            } else {
                file = toml::from_str(&text)
                    .map_err(|_| "invalid fleet TOML (expected [[instance]] tables)")?;
            }
        }
        for url in cli.urls {
            file.instance.push(Target {
                url,
                name: String::new(),
                api_key: None,
                gpus: vec![],
            });
        }
        if file.instance.is_empty() {
            file.instance.push(Target {
                url: std::env::var("TABBYAPI_ENDPOINT")
                    .unwrap_or_else(|_| "http://localhost:8080".into()),
                name: String::new(),
                api_key: None,
                gpus: vec![],
            });
        }
        let mut seen = HashSet::new();
        let mut targets = Vec::new();
        let key = cli.api_key.or_else(|| std::env::var("VLLM_API_KEY").ok());
        for mut t in file.instance {
            t.url = normalize_url(&t.url)?;
            if seen.insert(t.url.clone()) {
                t.name = if t.name.is_empty() {
                    t.url.clone()
                } else {
                    safe_text(&t.name)
                };
                if key.is_some() {
                    t.api_key = key.clone();
                }
                targets.push(t);
            }
        }
        if targets.len() > 32 {
            return Err("at most 32 targets are supported".into());
        }
        let interval = cli.interval.or(file.interval).unwrap_or(1.);
        if !interval.is_finite() || !(0.1..=60.).contains(&interval) {
            return Err("interval must be 0.1..60 seconds".into());
        }
        if !cli.timeout.is_finite() || !(0.05..=60.).contains(&cli.timeout) {
            return Err("timeout must be 0.05..60 seconds".into());
        }
        if !cli.metrics_path.starts_with('/')
            || cli.metrics_path.starts_with("//")
            || cli.metrics_path.chars().any(char::is_control)
            || cli.metrics_path.contains(['?', '#'])
        {
            return Err("metrics-path must be an absolute path without query or fragment".into());
        }
        let gpu: String = if cli.no_gpu || file.gpu == Some(false) {
            "none".into()
        } else {
            cli.gpu.unwrap_or_else(|| "local".into())
        };
        let gpu_host = cli.gpu_host.unwrap_or_default();
        if gpu == "ssh" && gpu_host.is_empty() {
            return Err("--gpu ssh requires --gpu-host HOST".into());
        }
        if gpu_host.starts_with('-')
            || gpu_host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err("invalid SSH GPU host".into());
        }
        Ok(Self {
            targets,
            interval,
            timeout: Duration::from_secs_f64(cli.timeout),
            path: cli.metrics_path,
            gpu,
            gpu_host,
            mock: cli.mock,
            once: cli.once,
            json: cli.json,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation() {
        assert!(normalize_url("http://user:secret@host").is_err());
        assert!(normalize_url("file:///tmp/x").is_err());
        assert_eq!(
            normalize_url("https://host/prefix/").unwrap(),
            "https://host/prefix"
        );
    }
    #[test]
    fn yaml() {
        assert_eq!(
            yaml_url("network:\n  host: 0.0.0.0\n  port: 8088\n").unwrap(),
            "http://127.0.0.1:8088"
        );
    }
    #[test]
    fn fleet_dedupe() {
        let c = Config::from_cli(Cli::parse_from([
            "monitor",
            "--url",
            "http://localhost:1",
            "--url",
            "http://localhost:1/",
        ]))
        .unwrap();
        assert_eq!(c.targets.len(), 1);
    }
    #[test]
    fn rejects_nan() {
        assert!(Config::from_cli(Cli::parse_from(["monitor", "--interval", "NaN"])).is_err());
    }
}

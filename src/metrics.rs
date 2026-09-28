use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug, Default, Serialize)]
pub struct Histogram {
    #[serde(serialize_with = "serialize_buckets")]
    pub buckets: Vec<(f64, f64)>,
    pub count: Option<f64>,
    pub sum: Option<f64>,
}
fn serialize_buckets<S: serde::Serializer>(
    buckets: &[(f64, f64)],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let values: Vec<(String, f64)> = buckets
        .iter()
        .map(|(bound, count)| {
            (
                if bound.is_infinite() {
                    "+Inf".into()
                } else {
                    bound.to_string()
                },
                *count,
            )
        })
        .collect();
    values.serialize(serializer)
}
impl Histogram {
    pub fn quantile(&self, q: f64) -> Option<f64> {
        let count = self.count?;
        if count <= 0. || self.buckets.is_empty() {
            return None;
        }
        let target = count * q;
        let (mut lower, mut before) = (0., 0.);
        for &(upper, n) in &self.buckets {
            if n >= target {
                if !upper.is_finite() {
                    return if lower > 0. { Some(lower) } else { None };
                }
                if n <= before {
                    return Some(upper);
                }
                return Some(lower + (upper - lower) * (target - before) / (n - before));
            }
            lower = upper;
            before = n;
        }
        None
    }
    fn delta(&self, old: &Self) -> Option<Self> {
        if self.buckets.len() != old.buckets.len() {
            return None;
        }
        let mut result = Self {
            count: Some(delta(self.count, old.count)?),
            sum: Some(delta(self.sum, old.sum)?),
            ..Self::default()
        };
        for (&(le, n), &(old_le, old_n)) in self.buckets.iter().zip(&old.buckets) {
            if le != old_le || n < old_n {
                return None;
            }
            result.buckets.push((le, n - old_n));
        }
        Some(result)
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Snapshot {
    pub loaded: Option<f64>,
    pub queued: Option<f64>,
    pub prefill: Option<f64>,
    pub decode: Option<f64>,
    pub completed: Option<f64>,
    pub cancelled: Option<f64>,
    pub errors: Option<f64>,
    pub computed: Option<f64>,
    pub cached: Option<f64>,
    pub output: Option<f64>,
    pub capacity: Option<f64>,
    pub used: Option<f64>,
    pub reusable: Option<f64>,
    pub accepted: Option<f64>,
    pub rejected: Option<f64>,
    pub observation_errors: Option<f64>,
    pub histograms: BTreeMap<String, Histogram>,
}
impl Snapshot {
    pub fn running(&self) -> Option<f64> {
        Some(self.prefill? + self.decode?)
    }
    pub fn kv_percent(&self) -> Option<f64> {
        ratio(self.used, self.capacity).map(|v| v * 100.)
    }
    pub fn mtp_percent(&self) -> Option<f64> {
        ratio(self.accepted, Some(self.accepted? + self.rejected?)).map(|v| v * 100.)
    }
    pub fn cache_share(&self) -> Option<f64> {
        ratio(self.cached, Some(self.computed? + self.cached?)).map(|v| v * 100.)
    }
}
pub fn ratio(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    let (a, b) = (a?, b?);
    if b > 0. { Some(a / b) } else { None }
}
pub fn delta(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    let (a, b) = (a?, b?);
    if a >= b { Some(a - b) } else { None }
}

// Parse the bounded contract, retaining only named metric groups. Unknown families
// are ignored. Label escapes are decoded before selection; duplicates are invalid.
fn labels(input: &str) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    let mut rest = input.trim();
    while !rest.is_empty() {
        let (key, tail) = rest.split_once('=').ok_or("invalid metric labels")?;
        let key = key.trim();
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err("invalid label key".into());
        }
        let tail = tail.trim_start();
        if !tail.starts_with('"') {
            return Err("unquoted metric label".into());
        }
        let mut val = String::new();
        let mut escape = false;
        let mut end = None;
        for (i, c) in tail[1..].char_indices() {
            if escape {
                val.push(match c {
                    'n' => '\n',
                    '"' => '"',
                    '\\' => '\\',
                    _ => return Err("invalid label escape".into()),
                });
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                end = Some(i + 2);
                break;
            } else {
                val.push(c);
            }
        }
        let end = end.ok_or("unterminated label")?;
        if map.insert(key.to_owned(), val).is_some() {
            return Err("duplicate label".into());
        }
        rest = tail[end..].trim_start();
        if !rest.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or("invalid label separator")?
                .trim_start();
        }
    }
    Ok(map)
}
pub fn parse(text: &str) -> Result<Snapshot, String> {
    let mut s = Snapshot::default();
    let mut schema = false;
    let mut seen = std::collections::BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let end = line
            .find(|c: char| c == '{' || c.is_whitespace())
            .ok_or("invalid metric sample")?;
        let name = &line[..end];
        let tail = &line[end..];
        let (ls, value) = if tail.starts_with('{') {
            let mut quote = false;
            let mut escape = false;
            let mut close = None;
            for (i, c) in tail.char_indices().skip(1) {
                if escape {
                    escape = false;
                    continue;
                }
                if c == '\\' && quote {
                    escape = true;
                } else if c == '"' {
                    quote = !quote;
                } else if c == '}' && !quote {
                    close = Some(i);
                    break;
                }
            }
            let close = close.ok_or("unterminated metric labels")?;
            (labels(&tail[1..close])?, tail[close + 1..].trim())
        } else {
            (BTreeMap::new(), tail.trim())
        };
        let mut fields = value.split_whitespace();
        let n: f64 = fields
            .next()
            .ok_or("missing metric value")?
            .parse()
            .map_err(|_| "invalid metric value")?;
        if let Some(timestamp) = fields.next() {
            timestamp.parse::<f64>().map_err(|_| "invalid timestamp")?;
        }
        if fields.next().is_some() {
            return Err("extra metric fields".into());
        }
        if !name.starts_with("tabbyapi_") {
            continue;
        }
        if !n.is_finite() || n < 0. {
            return Err("invalid nonnegative metric value".into());
        }
        if !seen.insert((name.to_string(), ls.clone())) {
            return Err("duplicate metric sample".into());
        }
        let l = |key: &str| ls.get(key).map(String::as_str).unwrap_or("");
        match name {
            "tabbyapi_metrics_schema_info" => {
                if l("version") != "1" || n != 1. {
                    return Err("unsupported TabbyAPI metrics schema (expected version 1)".into());
                }
                schema = true;
            }
            "tabbyapi_model_loaded" => s.loaded = Some(n),
            "tabbyapi_requests_active" => match l("state") {
                "queued" => s.queued = Some(n),
                "prefill" => s.prefill = Some(n),
                "decode" => s.decode = Some(n),
                _ => {}
            },
            "tabbyapi_requests_total" => match l("outcome") {
                "completed" => s.completed = Some(n),
                "cancelled" => s.cancelled = Some(n),
                "error" => s.errors = Some(n),
                _ => {}
            },
            "tabbyapi_prompt_tokens_total" => match l("source") {
                "computed" => s.computed = Some(n),
                "cached" => s.cached = Some(n),
                _ => {}
            },
            "tabbyapi_output_tokens_total" => s.output = Some(n),
            "tabbyapi_kv_cache_capacity_tokens" => s.capacity = Some(n),
            "tabbyapi_kv_cache_used_tokens" => s.used = Some(n),
            "tabbyapi_kv_cache_reusable_tokens" => s.reusable = Some(n),
            "tabbyapi_mtp_draft_tokens_total" => match l("outcome") {
                "accepted" => s.accepted = Some(n),
                "rejected" => s.rejected = Some(n),
                _ => {}
            },
            "tabbyapi_metrics_observation_errors_total" => s.observation_errors = Some(n),
            _ => {
                let key = if name.starts_with("tabbyapi_engine_time_to_first_token_seconds_") {
                    Some("ttft")
                } else if name.starts_with("tabbyapi_engine_phase_duration_seconds_") {
                    match l("phase") {
                        "queue" => Some("queue"),
                        "prefill" => Some("prefill"),
                        "decode" => Some("decode"),
                        "total" => Some("total"),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(key) = key {
                    let h = s.histograms.entry(key.into()).or_default();
                    if name.ends_with("_bucket") {
                        let le = if l("le") == "+Inf" {
                            f64::INFINITY
                        } else {
                            l("le").parse().map_err(|_| "invalid histogram boundary")?
                        };
                        if le.is_nan() || le < 0. {
                            return Err("invalid histogram boundary".into());
                        }
                        h.buckets.push((le, n));
                    } else if name.ends_with("_count") {
                        h.count = Some(n);
                    } else if name.ends_with("_sum") {
                        h.sum = Some(n);
                    }
                }
            }
        }
    }
    if !schema {
        return Err(
            "TabbyAPI metrics schema missing; install the schema-v1 /metrics extension".into(),
        );
    }
    for h in s.histograms.values_mut() {
        h.buckets.sort_by(|a, b| a.0.total_cmp(&b.0));
        if h.buckets
            .windows(2)
            .any(|w| w[0].0 == w[1].0 || w[0].1 > w[1].1)
        {
            return Err("invalid cumulative histogram".into());
        }
        if let (Some(count), Some(&(le, n))) = (h.count, h.buckets.last()) {
            if le.is_infinite() && count != n {
                return Err("histogram count mismatch".into());
            }
        }
    }
    if let (Some(c), Some(u), Some(r)) = (s.capacity, s.used, s.reusable) {
        if u > c || r > c {
            return Err("incoherent cache gauges".into());
        }
    }
    Ok(s)
}

#[derive(Clone, Default, Debug, Serialize)]
pub struct Rates {
    pub output: Option<f64>,
    pub computed: Option<f64>,
    pub cached: Option<f64>,
    pub jobs: Option<f64>,
}
#[derive(Clone, Default, Debug, Serialize)]
pub struct Session {
    pub busy_seconds: f64,
    pub idle_seconds: f64,
    pub unknown_seconds: f64,
    pub output: Option<f64>,
    pub computed: Option<f64>,
    pub cached: Option<f64>,
    pub completed: Option<f64>,
    pub peak_running: f64,
    pub peak_queued: f64,
}
/// One chart column per poll; `None` fields render as a gap.
#[derive(Clone, Copy, Debug, Default)]
pub struct Point {
    pub time: f64,
    pub output: Option<f64>,
    pub computed: Option<f64>,
    pub running: Option<f64>,
    pub queued: Option<f64>,
}
#[derive(Clone, Debug)]
pub struct Observation {
    pub time: f64,
    pub snapshot: Snapshot,
}
#[derive(Clone, Debug)]
pub struct State {
    pub last: Option<Observation>,
    pub error: Option<String>,
    pub rates: Rates,
    pub session: Session,
    pub history: VecDeque<Point>,
    pub window: VecDeque<Observation>,
    pub histograms: BTreeMap<String, Histogram>,
    pub gap: bool,
    // Consecutive failed polls; a single failure is bridged, not charted as a gap.
    failures: u32,
    pending_failure: Option<f64>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            last: None,
            error: None,
            rates: Rates::default(),
            session: Session::default(),
            history: VecDeque::new(),
            window: VecDeque::new(),
            histograms: BTreeMap::new(),
            gap: false,
            failures: 0,
            pending_failure: None,
        }
    }
}
impl State {
    pub fn reset(&mut self) {
        self.session = Session::default();
        self.last = None;
        self.rates = Rates::default();
        self.window.clear();
        self.histograms.clear();
        self.history.clear();
        self.gap = false;
        self.failures = 0;
        self.pending_failure = None;
    }
    // One transient failure keeps the previous sample as the rate baseline, so the
    // next success averages over the missed interval. Sustained outages become gaps.
    pub fn fail(&mut self, time: f64, error: String) {
        self.error = Some(error);
        self.rates = Rates::default();
        self.failures += 1;
        if self.failures == 1 {
            self.pending_failure = Some(time);
        } else {
            for t in self.pending_failure.take().into_iter().chain([time]) {
                self.history.push_back(Point {
                    time: t,
                    ..Point::default()
                });
            }
            self.gap = true;
        }
        self.trim(time);
    }
    pub fn trim(&mut self, time: f64) {
        while self.history.front().is_some_and(|v| time - v.time > 120.)
            || self.history.len() > 1200
        {
            self.history.pop_front();
        }
        while self.window.front().is_some_and(|v| time - v.time > 120.) || self.window.len() > 1200
        {
            self.window.pop_front();
        }
    }
    pub fn update(&mut self, time: f64, s: Snapshot) {
        self.error = None;
        self.rates = Rates::default();
        if let Some(old) = &self.last {
            let dt = time - old.time;
            let reset = [
                (s.output, old.snapshot.output),
                (s.computed, old.snapshot.computed),
                (s.cached, old.snapshot.cached),
                (s.completed, old.snapshot.completed),
                (s.cancelled, old.snapshot.cancelled),
                (s.errors, old.snapshot.errors),
            ]
            .iter()
            .any(|(a, b)| matches!((a,b),(Some(a),Some(b)) if a<b));
            if reset {
                self.window.clear();
                self.histograms.clear();
            }
            if dt > 0. && !self.gap && !reset {
                let out = delta(s.output, old.snapshot.output);
                let computed = delta(s.computed, old.snapshot.computed);
                let cached = delta(s.cached, old.snapshot.cached);
                let jobs = delta(s.completed, old.snapshot.completed);
                self.rates = Rates {
                    output: out.map(|v| v / dt),
                    computed: computed.map(|v| v / dt),
                    cached: cached.map(|v| v / dt),
                    jobs: jobs.map(|v| v / dt),
                };
                let busy = match (s.running(), old.snapshot.running()) {
                    (Some(a), Some(b)) => {
                        Some(a > 0. || b > 0. || out.unwrap_or(0.) > 0. || jobs.unwrap_or(0.) > 0.)
                    }
                    _ => None,
                };
                match busy {
                    Some(true) => self.session.busy_seconds += dt,
                    Some(false) => self.session.idle_seconds += dt,
                    None => self.session.unknown_seconds += dt,
                }
                if let Some(v) = out {
                    *self.session.output.get_or_insert(0.) += v;
                }
                if let Some(v) = computed {
                    *self.session.computed.get_or_insert(0.) += v;
                }
                if let Some(v) = cached {
                    *self.session.cached.get_or_insert(0.) += v;
                }
                if let Some(v) = jobs {
                    *self.session.completed.get_or_insert(0.) += v;
                }
            } else if dt > 0. {
                self.session.unknown_seconds += dt;
            }
        }
        if let Some(v) = s.running() {
            self.session.peak_running = self.session.peak_running.max(v);
        }
        if let Some(v) = s.queued {
            self.session.peak_queued = self.session.peak_queued.max(v);
        }
        self.trim(time);
        self.histograms.clear();
        if !self.gap {
            if let Some(old) = self.window.front() {
                for (name, h) in &s.histograms {
                    if let Some(d) = old.snapshot.histograms.get(name).and_then(|v| h.delta(v)) {
                        self.histograms.insert(name.clone(), d);
                    }
                }
            }
        }
        if self.gap {
            self.window.clear();
        }
        self.history.push_back(Point {
            time,
            output: self.rates.output,
            computed: self.rates.computed,
            running: s.running(),
            queued: s.queued,
        });
        let observation = Observation { time, snapshot: s };
        self.last = Some(observation.clone());
        self.window.push_back(observation);
        self.gap = false;
        self.failures = 0;
        self.pending_failure = None;
        self.trim(time);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const BASE: &str = "tabbyapi_metrics_schema_info{version=\"1\"} 1\n";
    #[test]
    fn real_fixture() {
        let s = parse(include_str!("../tests/fixtures/metrics.prom")).unwrap();
        assert_eq!(s.loaded, Some(1.));
        assert!(s.output.unwrap() > 0.);
        assert!(s.histograms["ttft"].quantile(0.5).is_some());
    }
    #[test]
    fn zero_missing() {
        let s = parse(&(BASE.to_owned() + "tabbyapi_output_tokens_total 0\n")).unwrap();
        assert_eq!(s.output, Some(0.));
        assert_eq!(s.used, None);
    }
    #[test]
    fn bad_schema_and_values() {
        assert!(parse("tabbyapi_metrics_schema_info{version=\"2\"} 1").is_err());
        for value in ["NaN", "-1", "inf"] {
            assert!(parse(&format!("{BASE}tabbyapi_output_tokens_total {value}")).is_err());
        }
        assert!(parse("garbage").is_err());
    }
    #[test]
    fn duplicate_and_labels() {
        assert!(parse(&(BASE.to_owned() + BASE)).is_err());
        assert_eq!(
            labels("a=\"one\\\"two\",b=\"x\\ny\"").unwrap()["a"],
            "one\"two"
        );
    }
    #[test]
    fn histogram() {
        let h = Histogram {
            buckets: vec![(1., 2.), (2., 4.), (f64::INFINITY, 4.)],
            count: Some(4.),
            sum: Some(5.),
        };
        assert_eq!(h.quantile(0.5), Some(1.));
        assert_eq!(h.quantile(0.75), Some(1.5));
    }
    fn snap(v: f64) -> Snapshot {
        Snapshot {
            output: Some(v),
            computed: Some(v),
            cached: Some(0.),
            completed: Some(v / 10.),
            queued: Some(0.),
            prefill: Some(0.),
            decode: Some(1.),
            ..Default::default()
        }
    }
    #[test]
    fn rates_reset_gap() {
        let mut st = State::default();
        st.update(1., snap(10.));
        assert!(st.rates.output.is_none());
        st.update(2., snap(20.));
        assert_eq!(st.rates.output, Some(10.));
        st.update(3., snap(1.));
        assert!(st.rates.output.is_none());
        st.fail(4., "timeout".into());
        st.fail(5., "timeout".into());
        st.update(6., snap(21.));
        assert!(st.rates.output.is_none());
        st.update(7., snap(31.));
        assert_eq!(st.rates.output, Some(10.));
        assert_eq!(st.session.output, Some(20.));
        let gaps = st.history.iter().filter(|p| p.running.is_none()).count();
        assert_eq!(gaps, 2, "sustained outage charts each failed poll");
    }
    #[test]
    fn single_failure_is_bridged() {
        let mut st = State::default();
        st.update(1., snap(10.));
        st.update(2., snap(20.));
        st.fail(3., "timeout".into());
        assert!(st.error.is_some());
        st.update(4., snap(40.));
        assert!(st.error.is_none());
        assert_eq!(st.rates.output, Some(10.));
        assert_eq!(st.session.output, Some(30.));
        let points: Vec<_> = st.history.iter().skip(1).collect();
        assert_eq!(points.len(), 2);
        assert!(
            points
                .iter()
                .all(|p| p.output.is_some() && p.running.is_some())
        );
    }
    #[test]
    fn history_bounded() {
        let mut st = State::default();
        for i in 0..10000 {
            st.update(i as f64 / 10., snap(i as f64));
        }
        assert!(st.history.len() <= 1200);
        assert!(st.window.len() <= 1200);
        st.reset();
        assert!(st.history.is_empty());
        assert_eq!(st.session.output, None);
    }
    #[test]
    fn sessions_do_not_invent_missing_work() {
        let mut st = State::default();
        st.update(1., Snapshot::default());
        st.update(2., Snapshot::default());
        assert_eq!(st.session.output, None);
        assert_eq!(st.rates.output, None);
        assert_eq!(st.session.unknown_seconds, 1.);
    }
    #[test]
    fn end_of_job_is_not_lost_to_idle_sampling() {
        let mut st = State::default();
        let mut a = snap(0.);
        a.decode = Some(0.);
        let mut b = snap(10.);
        b.decode = Some(0.);
        st.update(1., a);
        st.update(2., b);
        assert_eq!(st.session.completed, Some(1.));
        assert_eq!(st.session.busy_seconds, 1.);
    }
    #[test]
    fn histogram_window_and_restart() {
        let mut a = snap(10.);
        a.histograms.insert(
            "ttft".into(),
            Histogram {
                buckets: vec![(1., 10.), (f64::INFINITY, 10.)],
                count: Some(10.),
                sum: Some(5.),
            },
        );
        let mut b = snap(20.);
        b.histograms.insert(
            "ttft".into(),
            Histogram {
                buckets: vec![(1., 12.), (f64::INFINITY, 12.)],
                count: Some(12.),
                sum: Some(6.),
            },
        );
        let mut st = State::default();
        st.update(0., a.clone());
        assert!(st.histograms.is_empty());
        st.update(1., b);
        assert_eq!(st.histograms["ttft"].count, Some(2.));
        st.update(2., a);
        assert!(st.histograms.is_empty());
    }
    #[test]
    fn infinite_bucket_is_explicit_in_json() {
        let h = Histogram {
            buckets: vec![(f64::INFINITY, 2.)],
            count: Some(2.),
            sum: Some(1.),
        };
        assert!(serde_json::to_string(&h).unwrap().contains("+Inf"));
    }
}

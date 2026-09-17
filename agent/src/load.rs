//! Load hint + scale/limit recommend (USE + golden-signal lite).

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoadHint {
    Ok,
    Watch,
    Tight,
}

impl LoadHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Watch => "watch",
            Self::Tight => "tight",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Recommend {
    Ok,
    Watch,
    ScaleOut,
    Limit,
}

impl Recommend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Watch => "watch",
            Self::ScaleOut => "scale_out",
            Self::Limit => "limit",
        }
    }
}

pub fn mem_pct(used: Option<i64>, limit: Option<i64>) -> Option<f64> {
    match (used, limit) {
        (Some(u), Some(l)) if l > 0 => Some((u as f64 / l as f64) * 100.0),
        _ => None,
    }
}

pub fn resource_load_hint(cpu_pct: Option<f64>, mem_pct: Option<f64>) -> Option<LoadHint> {
    let mut hint: Option<LoadHint> = None;
    for v in [cpu_pct, mem_pct].into_iter().flatten() {
        let h = if v >= 85.0 {
            LoadHint::Tight
        } else if v >= 70.0 {
            LoadHint::Watch
        } else {
            LoadHint::Ok
        };
        hint = Some(match hint {
            Some(prev) => prev.max(h),
            None => h,
        });
    }
    hint
}

/// Parse Elasticsearch-style `_cluster/health` JSON (or similar).
pub fn parse_es_health_hint(body: &str) -> Option<LoadHint> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if !v.get("status").is_some() {
        return None;
    }
    let mut hint = LoadHint::Ok;
    if let Some(status) = v.get("status").and_then(|s| s.as_str()) {
        match status.to_lowercase().as_str() {
            "yellow" => hint = hint.max(LoadHint::Watch),
            "red" => hint = hint.max(LoadHint::Tight),
            _ => {}
        }
    }
    if v.get("timed_out").and_then(|t| t.as_bool()) == Some(true) {
        hint = hint.max(LoadHint::Tight);
    }
    if let Some(pending) = v
        .get("number_of_pending_tasks")
        .and_then(|p| p.as_u64().or_else(|| p.as_i64().map(|i| i as u64)))
    {
        if pending >= 20 {
            hint = hint.max(LoadHint::Tight);
        } else if pending >= 5 {
            hint = hint.max(LoadHint::Watch);
        }
    }
    Some(hint)
}

pub fn looks_like_cluster_health_url(url: &str) -> bool {
    url.contains("/_cluster/health")
}

pub fn merge_hint(a: Option<LoadHint>, b: Option<LoadHint>) -> Option<LoadHint> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

pub fn service_recommend(
    status: &str,
    load_hint: Option<LoadHint>,
    latency_ms: Option<i64>,
) -> Recommend {
    let status = status.to_lowercase();
    if status == "down" || status == "unknown" {
        return Recommend::Limit;
    }

    let latency_high = latency_ms.is_some_and(|l| l >= 2000);

    match load_hint {
        Some(LoadHint::Tight) if latency_high => Recommend::Limit,
        Some(LoadHint::Tight) => Recommend::ScaleOut,
        Some(LoadHint::Watch) => Recommend::Watch,
        Some(LoadHint::Ok) | None => {
            if latency_high {
                Recommend::Watch
            } else {
                Recommend::Ok
            }
        }
    }
}

/// ES red / high pending → prefer limit (protect cluster).
pub fn apply_es_recommend(hint: Option<LoadHint>, base: Recommend) -> Recommend {
    match hint {
        Some(LoadHint::Tight) => Recommend::Limit.max(base),
        _ => base,
    }
}

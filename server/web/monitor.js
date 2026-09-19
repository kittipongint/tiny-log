const hostRows = document.getElementById("host-rows");
const serviceRows = document.getElementById("service-rows");
const hostDetail = document.getElementById("host-detail");
const detailTitle = document.getElementById("detail-title");
const detailMeta = document.getElementById("detail-meta");
const detailForecast = document.getElementById("detail-forecast");
const navUser = document.getElementById("nav-user");
const navBadge = document.getElementById("nav-badge");
const logoutBtn = document.getElementById("logout-btn");

let selectedHost = null;
let hostsCache = [];

async function api(path, options = {}) {
  const res = await fetch(path, {
    credentials: "same-origin",
    ...options,
    headers: {
      ...(options.body ? { "Content-Type": "application/json" } : {}),
      ...(options.headers || {}),
    },
  });
  if (res.status === 401) {
    window.location.href = "/login";
    throw new Error("unauthorized");
  }
  return res;
}

function fmtBytes(n) {
  if (n == null) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = Number(n);
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

/** Same unit for both sides: "11.12/128.0 MB" */
function fmtBytesPair(used, total) {
  if (used == null || total == null || Number(total) <= 0) return null;
  const units = ["B", "KB", "MB", "GB", "TB"];
  let scale = Number(total);
  let i = 0;
  while (scale >= 1024 && i < units.length - 1) {
    scale /= 1024;
    i += 1;
  }
  const div = 1024 ** i;
  const u = Number(used) / div;
  const t = Number(total) / div;
  if (i === 0) return `${u.toFixed(0)}/${t.toFixed(0)} ${units[i]}`;
  return `${u.toFixed(2)}/${t.toFixed(1)} ${units[i]}`;
}

function fmtPct(n) {
  if (n == null || Number.isNaN(Number(n))) return "—";
  return `${Number(n).toFixed(1)}%`;
}

/** Same unit for both sides: "0.44/4.0 cores" */
function fmtCoresPair(used, total) {
  if (used == null || total == null || Number(total) <= 0) return null;
  return `${Number(used).toFixed(2)}/${Number(total).toFixed(1)} cores`;
}

function fmtUsage(pct, used, total, formatPair = fmtBytesPair) {
  const pair = formatPair(used, total);
  const p =
    pct != null && !Number.isNaN(Number(pct))
      ? Number(pct)
      : used != null && total
        ? (Number(used) / Number(total)) * 100
        : null;
  if (pair && p != null) return `${pair} (${p.toFixed(0)}%)`;
  if (pair) return pair;
  return fmtPct(p);
}

function fmtTime(iso) {
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso || "—";
  }
}

function barLevel(pct) {
  if (pct == null || Number.isNaN(Number(pct))) return "unknown";
  const v = Number(pct);
  if (v >= 85) return "hot";
  if (v >= 70) return "warm";
  return "ok";
}

function metricCell(pct, used, total, formatPair = fmtBytesPair) {
  const wrap = document.createElement("div");
  wrap.className = "metric-cell";
  const resolvedPct =
    pct != null && !Number.isNaN(Number(pct))
      ? Number(pct)
      : used != null && total
        ? (Number(used) / Number(total)) * 100
        : null;
  const label = document.createElement("span");
  label.className = "metric-pct";
  label.textContent = fmtUsage(resolvedPct, used, total, formatPair);
  const track = document.createElement("div");
  track.className = `metric-bar metric-bar-${barLevel(resolvedPct)}`;
  const fill = document.createElement("span");
  fill.style.width = `${Math.min(100, Math.max(0, resolvedPct || 0))}%`;
  track.appendChild(fill);
  wrap.append(label, track);
  return wrap;
}

/** Host: cpu_pct is 0–100 of whole machine. Service: cpu_pct is core-% (may exceed 100). */
function cpuMetricCell(pct, nCpus, mode = "host") {
  if (pct == null || Number.isNaN(Number(pct))) return metricCell(null);
  const p = Number(pct);
  if (nCpus == null || Number(nCpus) <= 0) return metricCell(p);
  const n = Number(nCpus);
  const used = mode === "host" ? (p / 100) * n : p / 100;
  const displayPct = mode === "host" ? p : (used / n) * 100;
  return metricCell(displayPct, used, n, fmtCoresPair);
}

function badge(kind, value) {
  const span = document.createElement("span");
  const v = String(value || "—").toLowerCase();
  span.className = `badge badge-${kind}-${v.replace(/_/g, "-")}`;
  span.textContent = String(value || "—").toUpperCase().replace(/_/g, " ");
  return span;
}

function forecastLine(f) {
  if (!f) return "insufficient history";
  const parts = [];
  if (f.cpu_pct_1h != null) parts.push(`CPU ~${Number(f.cpu_pct_1h).toFixed(0)}% in 1h`);
  if (f.mem_pct_1h != null) parts.push(`Mem ~${Number(f.mem_pct_1h).toFixed(0)}% in 1h`);
  if (f.eta_hours_to_cpu_85 != null) {
    parts.push(`CPU→85% ~${Number(f.eta_hours_to_cpu_85).toFixed(1)}h`);
  }
  if (f.eta_hours_to_mem_85 != null) {
    parts.push(`Mem→85% ~${Number(f.eta_hours_to_mem_85).toFixed(1)}h`);
  }
  return parts.length ? parts.join(" · ") : "insufficient history";
}

function sparkline(values, color) {
  const w = 280;
  const h = 56;
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
  svg.setAttribute("class", "spark-svg");
  svg.setAttribute("preserveAspectRatio", "none");
  if (!values.length) {
    const t = document.createElementNS("http://www.w3.org/2000/svg", "text");
    t.setAttribute("x", "8");
    t.setAttribute("y", "30");
    t.setAttribute("fill", "currentColor");
    t.setAttribute("font-size", "11");
    t.textContent = "no data";
    svg.appendChild(t);
    return svg;
  }
  const max = Math.max(100, ...values.map((v) => v ?? 0));
  const min = 0;
  const n = values.length;
  const pts = values.map((v, i) => {
    const x = n === 1 ? 0 : (i / (n - 1)) * w;
    const y = h - ((Number(v) - min) / (max - min || 1)) * (h - 4) - 2;
    return [x, y];
  });
  const line = pts.map(([x, y], i) => `${i === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  const area = `${line} L${w},${h} L0,${h} Z`;
  const a = document.createElementNS("http://www.w3.org/2000/svg", "path");
  a.setAttribute("d", area);
  a.setAttribute("fill", color);
  a.setAttribute("opacity", "0.18");
  const p = document.createElementNS("http://www.w3.org/2000/svg", "path");
  p.setAttribute("d", line);
  p.setAttribute("fill", "none");
  p.setAttribute("stroke", color);
  p.setAttribute("stroke-width", "1.5");
  svg.append(a, p);
  return svg;
}

function hostDisks(h) {
  if (Array.isArray(h.disks) && h.disks.length) return h.disks;
  if (h.disk_used_bytes != null || h.disk_total_bytes != null) {
    return [
      {
        name: "disk",
        used_bytes: h.disk_used_bytes,
        total_bytes: h.disk_total_bytes,
      },
    ];
  }
  return [];
}

function appendDiskMetric(td, disk) {
  const wrap = document.createElement("div");
  wrap.className = "disk-metric";
  const name = document.createElement("span");
  name.className = "disk-metric-name muted";
  name.textContent = disk.name || disk.path || "disk";
  if (disk.path) name.title = disk.path;
  const pct =
    disk.used_bytes != null && disk.total_bytes
      ? (Number(disk.used_bytes) / Number(disk.total_bytes)) * 100
      : null;
  wrap.append(name, metricCell(pct, disk.used_bytes, disk.total_bytes));
  td.appendChild(wrap);
}

function renderHosts(hosts) {
  hostsCache = hosts;
  hostRows.replaceChildren();
  if (!hosts.length) {
    const tr = document.createElement("tr");
    const td = document.createElement("td");
    td.colSpan = 6;
    td.className = "muted";
    td.textContent = "No host samples yet. Start tiny-log-agent on a host.";
    tr.appendChild(td);
    hostRows.appendChild(tr);
    hostDetail.hidden = true;
    return;
  }
  for (const h of hosts) {
    const disks = hostDisks(h);
    const tr = document.createElement("tr");
    tr.className = "systems-row";
    if (selectedHost === h.host) tr.classList.add("selected");
    tr.tabIndex = 0;
    tr.addEventListener("click", () => selectHost(h.host));
    tr.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        selectHost(h.host);
      }
    });

    const tdSys = document.createElement("td");
    const sysWrap = document.createElement("div");
    sysWrap.className = "system-name";
    const dot = document.createElement("span");
    dot.className = `status-dot status-dot-${(h.recommend || "ok").replace(/_/g, "-")}`;
    const name = document.createElement("span");
    name.textContent = h.host || "";
    sysWrap.append(dot, name);
    tdSys.appendChild(sysWrap);

    const tdCpu = document.createElement("td");
    tdCpu.appendChild(cpuMetricCell(h.cpu_pct, h.n_cpus, "host"));
    const tdMem = document.createElement("td");
    tdMem.appendChild(metricCell(h.mem_pct, h.mem_used_bytes, h.mem_total_bytes));
    const tdDisk = document.createElement("td");
    if (disks.length <= 1) {
      const d = disks[0];
      if (d) {
        tdDisk.appendChild(
          metricCell(
            h.disk_pct,
            d.used_bytes ?? h.disk_used_bytes,
            d.total_bytes ?? h.disk_total_bytes
          )
        );
      } else {
        tdDisk.appendChild(
          metricCell(h.disk_pct, h.disk_used_bytes, h.disk_total_bytes)
        );
      }
    } else {
      tdDisk.className = "muted mono";
      tdDisk.textContent = `${disks.length} disks`;
    }

    const tdLoad = document.createElement("td");
    tdLoad.className = "mono";
    tdLoad.textContent =
      h.load_per_cpu != null
        ? Number(h.load_per_cpu).toFixed(2)
        : h.load1 != null
          ? Number(h.load1).toFixed(2)
          : "—";

    const tdRec = document.createElement("td");
    tdRec.appendChild(badge("rec", h.recommend || "ok"));

    tr.append(tdSys, tdCpu, tdMem, tdDisk, tdLoad, tdRec);
    hostRows.appendChild(tr);

    if (disks.length > 1) {
      for (const d of disks) {
        const dtr = document.createElement("tr");
        dtr.className = "systems-disk-row";
        if (selectedHost === h.host) dtr.classList.add("selected");
        dtr.addEventListener("click", () => selectHost(h.host));

        const dSys = document.createElement("td");
        const label = document.createElement("div");
        label.className = "disk-row-label";
        label.textContent = d.name || "disk";
        if (d.path) label.title = d.path;
        dSys.appendChild(label);

        const empty = () => {
          const td = document.createElement("td");
          td.className = "muted";
          td.textContent = "";
          return td;
        };
        const dDisk = document.createElement("td");
        appendDiskMetric(dDisk, d);
        dtr.append(dSys, empty(), empty(), dDisk, empty(), empty());
        hostRows.appendChild(dtr);
      }
    }
  }
  if (selectedHost && !hosts.some((h) => h.host === selectedHost)) {
    selectedHost = null;
    hostDetail.hidden = true;
  }
}

function renderServices(services) {
  serviceRows.replaceChildren();
  const list = selectedHost
    ? services.filter((s) => s.host === selectedHost)
    : services;
  if (!list.length) {
    const tr = document.createElement("tr");
    const td = document.createElement("td");
    td.colSpan = 8;
    td.className = "muted";
    td.textContent = selectedHost
      ? "No service checks for this host."
      : "No service checks yet.";
    tr.appendChild(td);
    serviceRows.appendChild(tr);
    return;
  }
  for (const s of list) {
    const tr = document.createElement("tr");
    if (s.message) tr.title = s.message;
    const memPct =
      s.mem_used_bytes != null && s.mem_limit_bytes
        ? (s.mem_used_bytes / s.mem_limit_bytes) * 100
        : null;

    const tdHost = document.createElement("td");
    tdHost.textContent = s.host;
    const tdSvc = document.createElement("td");
    tdSvc.textContent = s.service;
    const tdStatus = document.createElement("td");
    tdStatus.appendChild(badge("status", s.status));
    const hostN =
      hostsCache.find((h) => h.host === s.host)?.n_cpus ?? null;
    const tdCpu = document.createElement("td");
    tdCpu.appendChild(cpuMetricCell(s.cpu_pct, hostN, "cores"));
    const tdMem = document.createElement("td");
    if (s.mem_used_bytes != null && s.mem_limit_bytes) {
      tdMem.appendChild(
        metricCell(memPct, s.mem_used_bytes, s.mem_limit_bytes)
      );
    } else if (s.mem_used_bytes != null) {
      tdMem.className = "mono";
      tdMem.textContent = fmtBytes(s.mem_used_bytes);
    } else {
      tdMem.appendChild(metricCell(null));
    }
    const tdLoad = document.createElement("td");
    tdLoad.appendChild(badge("load", s.load_hint || "—"));
    const tdRec = document.createElement("td");
    tdRec.appendChild(badge("rec", s.recommend || "—"));
    const tdLat = document.createElement("td");
    tdLat.textContent = s.latency_ms == null ? "—" : `${s.latency_ms} ms`;

    tr.append(tdHost, tdSvc, tdStatus, tdCpu, tdMem, tdLoad, tdRec, tdLat);
    serviceRows.appendChild(tr);
  }
}

async function selectHost(host) {
  selectedHost = host;
  for (const row of hostRows.querySelectorAll(".systems-row")) {
    row.classList.toggle(
      "selected",
      row.querySelector(".system-name span:last-child")?.textContent === host
    );
  }
  const h = hostsCache.find((x) => x.host === host);
  hostDetail.hidden = false;
  detailTitle.textContent = host;
  detailMeta.textContent = h
    ? `Updated ${fmtTime(h.timestamp)} · headroom ${fmtPct(h.headroom_pct)}`
    : "";
  const sparkFigs = document.querySelectorAll(".spark-card figcaption");
  if (h && sparkFigs.length >= 3) {
    const usedCores =
      h.cpu_pct != null && h.n_cpus
        ? (Number(h.cpu_pct) / 100) * Number(h.n_cpus)
        : null;
    sparkFigs[0].textContent = h.n_cpus
      ? `CPU · ${fmtUsage(h.cpu_pct, usedCores, h.n_cpus, fmtCoresPair)}`
      : `CPU · ${fmtPct(h.cpu_pct)}`;
    sparkFigs[1].textContent = `Memory · ${fmtUsage(
      h.mem_pct,
      h.mem_used_bytes,
      h.mem_total_bytes
    )}`;
    const disks = hostDisks(h);
    if (disks.length > 1) {
      sparkFigs[2].textContent = `Disk · ${disks
        .map((d) => {
          const p =
            d.used_bytes != null && d.total_bytes
              ? (Number(d.used_bytes) / Number(d.total_bytes)) * 100
              : null;
          return `${d.name} ${fmtUsage(p, d.used_bytes, d.total_bytes)}`;
        })
        .join(" · ")}`;
    } else {
      sparkFigs[2].textContent = `Disk · ${fmtUsage(
        h.disk_pct,
        h.disk_used_bytes,
        h.disk_total_bytes
      )}`;
    }
  }
  detailForecast.textContent = "Loading…";
  document.getElementById("spark-cpu").replaceChildren();
  document.getElementById("spark-mem").replaceChildren();
  document.getElementById("spark-disk").replaceChildren();

  const [histRes, capRes] = await Promise.all([
    api(
      `/api/v1/metrics/history?kind=host&host=${encodeURIComponent(host)}&limit=60`
    ),
    api(`/api/v1/metrics/capacity?host=${encodeURIComponent(host)}`),
  ]);

  if (histRes.ok) {
    const hist = await histRes.json();
    const samples = (hist.samples || []).slice().reverse();
    const cpu = samples.map((s) => s.cpu_pct ?? 0);
    const mem = samples.map((s) => {
      if (s.mem_used_bytes != null && s.mem_total_bytes) {
        return (s.mem_used_bytes / s.mem_total_bytes) * 100;
      }
      return 0;
    });
    const disk = samples.map((s) => {
      if (s.disk_used_bytes != null && s.disk_total_bytes) {
        return (s.disk_used_bytes / s.disk_total_bytes) * 100;
      }
      return 0;
    });
    document.getElementById("spark-cpu").appendChild(sparkline(cpu, "#3d9cf0"));
    document.getElementById("spark-mem").appendChild(sparkline(mem, "#3ecf8e"));
    document.getElementById("spark-disk").appendChild(sparkline(disk, "#e0a100"));
  }

  if (capRes.ok) {
    const cap = await capRes.json();
    detailForecast.textContent = forecastLine(cap.forecast);
  } else {
    detailForecast.textContent = "insufficient history";
  }

  const overview = window.__monitorServices || [];
  renderServices(overview);
}

async function loadOverview() {
  const res = await api("/api/v1/metrics/overview");
  if (!res.ok) {
    const msg =
      res.status === 401
        ? null
        : `Metrics API error (${res.status}). Check metrics.db / server logs.`;
    if (msg) {
      hostRows.replaceChildren();
      const tr = document.createElement("tr");
      const td = document.createElement("td");
      td.colSpan = 6;
      td.className = "muted";
      td.textContent = msg;
      tr.appendChild(td);
      hostRows.appendChild(tr);
      hostDetail.hidden = true;
      serviceRows.replaceChildren();
      return;
    }
    throw new Error("unauthorized");
  }
  const data = await res.json();
  window.__monitorServices = data.services || [];
  renderHosts(data.hosts || []);
  renderServices(window.__monitorServices);
  if (selectedHost) {
    await selectHost(selectedHost);
  }
}

logoutBtn.addEventListener("click", async () => {
  await api("/api/auth/logout", { method: "POST" });
  window.location.href = "/login";
});

async function boot() {
  const me = await api("/api/auth/me");
  if (!me.ok) return;
  const data = await me.json();
  navUser.textContent = data.username || "";
  if (data.auth_mode === "anonymous") {
    navBadge.hidden = false;
    navBadge.textContent = "anonymous";
    logoutBtn.hidden = true;
  } else if (data.setup_required) {
    window.location.href = "/setup";
    return;
  }
  await loadOverview();
  setInterval(() => {
    loadOverview().catch(console.error);
  }, 30000);
}

boot().catch(() => {
  window.location.href = "/login";
});

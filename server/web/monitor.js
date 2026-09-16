const hostCards = document.getElementById("host-cards");
const serviceRows = document.getElementById("service-rows");
const navUser = document.getElementById("nav-user");
const navBadge = document.getElementById("nav-badge");
const logoutBtn = document.getElementById("logout-btn");

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

function fmtPct(n) {
  if (n == null) return "—";
  return `${Number(n).toFixed(1)}%`;
}

function fmtTime(iso) {
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso || "—";
  }
}

function renderHosts(hosts) {
  hostCards.replaceChildren();
  if (!hosts.length) {
    const empty = document.createElement("p");
    empty.className = "muted";
    empty.textContent = "No host samples yet. Start tiny-log-agent on a host.";
    hostCards.appendChild(empty);
    return;
  }
  for (const h of hosts) {
    const card = document.createElement("article");
    card.className = "host-card";
    const title = document.createElement("h3");
    title.textContent = h.host || "";
    const meta = document.createElement("p");
    meta.className = "muted";
    meta.textContent = `Updated ${fmtTime(h.timestamp)}`;
    const grid = document.createElement("dl");
    const rows = [
      ["CPU", fmtPct(h.cpu_pct)],
      ["Memory", `${fmtBytes(h.mem_used_bytes)} / ${fmtBytes(h.mem_total_bytes)}`],
      ["Disk", `${fmtBytes(h.disk_used_bytes)} / ${fmtBytes(h.disk_total_bytes)}`],
      ["Load1", h.load1 == null ? "—" : Number(h.load1).toFixed(2)],
    ];
    for (const [k, v] of rows) {
      const dt = document.createElement("dt");
      dt.textContent = k;
      const dd = document.createElement("dd");
      dd.textContent = v;
      grid.append(dt, dd);
    }
    card.append(title, meta, grid);
    hostCards.appendChild(card);
  }
}

function renderServices(services) {
  serviceRows.replaceChildren();
  if (!services.length) {
    const tr = document.createElement("tr");
    const td = document.createElement("td");
    td.colSpan = 7;
    td.className = "muted";
    td.textContent = "No service checks yet.";
    tr.appendChild(td);
    serviceRows.appendChild(tr);
    return;
  }
  for (const s of services) {
    const tr = document.createElement("tr");
    const cells = [
      s.host,
      s.service,
      s.kind,
      s.status,
      s.latency_ms == null ? "—" : `${s.latency_ms} ms`,
      fmtTime(s.timestamp),
      s.message || "—",
    ];
    cells.forEach((text, idx) => {
      const td = document.createElement("td");
      if (idx === 3) {
        const span = document.createElement("span");
        span.className = `status status-${String(s.status || "").toLowerCase()}`;
        span.textContent = String(s.status || "").toUpperCase();
        td.appendChild(span);
      } else {
        td.textContent = text;
      }
      tr.appendChild(td);
    });
    serviceRows.appendChild(tr);
  }
}

async function loadOverview() {
  const res = await api("/api/v1/metrics/overview");
  if (!res.ok) throw new Error("overview");
  const data = await res.json();
  renderHosts(data.hosts || []);
  renderServices(data.services || []);
}

logoutBtn.addEventListener("click", async () => {
  await api("/api/auth/logout", { method: "POST" });
  window.location.href = "/login";
});

async function boot() {
  const me = await api("/api/auth/me");
  if (!me.ok) return;
  const data = await me.json();
  if (data.setup_required) {
    window.location.href = "/setup";
    return;
  }
  navUser.textContent = data.username || "";
  if (data.auth_mode === "anonymous") {
    navBadge.hidden = false;
    navBadge.textContent = "anonymous";
    logoutBtn.hidden = true;
  }
  await loadOverview();
  setInterval(() => {
    loadOverview().catch(console.error);
  }, 30000);
}

boot().catch(() => {
  window.location.href = "/login";
});

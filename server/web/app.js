const rowsEl = document.getElementById("log-rows");
const appFilter = document.getElementById("filter-app");
const levelFilter = document.getElementById("filter-level");
const searchInput = document.getElementById("filter-search");
const searchBtn = document.getElementById("search-btn");
const liveBtn = document.getElementById("live-btn");
const logoutBtn = document.getElementById("logout-btn");
const navUser = document.getElementById("nav-user");
const banner = document.getElementById("new-logs-banner");
const newLogsBtn = document.getElementById("new-logs-btn");
const dialog = document.getElementById("detail-dialog");

let eventSource = null;
let pendingNew = [];
let stickToTop = true;

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

function formatTime(iso) {
  try {
    const d = new Date(iso);
    return d.toLocaleTimeString([], { hour12: false });
  } catch {
    return iso;
  }
}

function levelClass(level) {
  return `level level-${String(level || "").toLowerCase()}`;
}

function createRow(log) {
  const tr = document.createElement("tr");
  tr.dataset.id = String(log.id);

  const tdTime = document.createElement("td");
  tdTime.textContent = formatTime(log.timestamp);

  const tdApp = document.createElement("td");
  tdApp.textContent = log.app || "";

  const tdLevel = document.createElement("td");
  const levelSpan = document.createElement("span");
  levelSpan.className = levelClass(log.level);
  levelSpan.textContent = String(log.level || "").toUpperCase();
  tdLevel.appendChild(levelSpan);

  const tdMsg = document.createElement("td");
  tdMsg.textContent = log.message || "";

  tr.append(tdTime, tdApp, tdLevel, tdMsg);
  tr.addEventListener("click", () => showDetail(log));
  return tr;
}

function prependLog(log) {
  const existing = rowsEl.querySelector(`[data-id="${log.id}"]`);
  if (existing) return;
  rowsEl.prepend(createRow(log));
}

function appendLogs(logs) {
  const frag = document.createDocumentFragment();
  for (const log of logs) {
    frag.appendChild(createRow(log));
  }
  rowsEl.appendChild(frag);
}

function showDetail(log) {
  document.getElementById("d-timestamp").textContent = log.timestamp || "";
  document.getElementById("d-app").textContent = log.app || "";
  document.getElementById("d-level").textContent = log.level || "";
  document.getElementById("d-source").textContent = log.source || "—";
  document.getElementById("d-message").textContent = log.message || "";
  const metaEl = document.getElementById("d-meta");
  metaEl.textContent = log.meta ? JSON.stringify(log.meta, null, 2) : "—";
  dialog.showModal();
}

function updateBanner() {
  if (pendingNew.length === 0) {
    banner.hidden = true;
    return;
  }
  banner.hidden = false;
  newLogsBtn.textContent = `${pendingNew.length} new log${pendingNew.length === 1 ? "" : "s"}`;
}

function isNearTop() {
  return window.scrollY < 80;
}

window.addEventListener("scroll", () => {
  stickToTop = isNearTop();
  if (stickToTop && pendingNew.length) {
    flushPending();
  }
});

function flushPending() {
  for (const log of pendingNew.reverse()) {
    prependLog(log);
  }
  pendingNew = [];
  updateBanner();
  window.scrollTo({ top: 0 });
}

newLogsBtn.addEventListener("click", flushPending);

function queryString() {
  const params = new URLSearchParams();
  if (appFilter.value) params.set("app", appFilter.value);
  if (levelFilter.value) params.set("level", levelFilter.value);
  if (searchInput.value.trim()) params.set("search", searchInput.value.trim());
  params.set("limit", "100");
  return params.toString();
}

function matchesFilters(log) {
  if (appFilter.value && log.app !== appFilter.value) return false;
  if (levelFilter.value && String(log.level).toLowerCase() !== levelFilter.value) return false;
  const q = searchInput.value.trim().toLowerCase();
  if (q && !String(log.message || "").toLowerCase().includes(q)) return false;
  return true;
}

async function loadLogs() {
  const res = await api(`/api/v1/logs?${queryString()}`);
  if (!res.ok) throw new Error("failed to load logs");
  const data = await res.json();
  rowsEl.replaceChildren();
  appendLogs(data.logs || []);
  pendingNew = [];
  updateBanner();
}

async function loadApps() {
  const res = await api("/api/v1/apps");
  if (!res.ok) return;
  const data = await res.json();
  const current = appFilter.value;
  appFilter.replaceChildren();
  const all = document.createElement("option");
  all.value = "";
  all.textContent = "All";
  appFilter.appendChild(all);
  for (const app of data.apps || []) {
    const opt = document.createElement("option");
    opt.value = app;
    opt.textContent = app;
    appFilter.appendChild(opt);
  }
  appFilter.value = current;
}

function stopLive() {
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }
  liveBtn.setAttribute("aria-pressed", "false");
  liveBtn.textContent = "Live";
}

function startLive() {
  stopLive();
  eventSource = new EventSource("/api/v1/logs/stream");
  liveBtn.setAttribute("aria-pressed", "true");
  liveBtn.textContent = "Live";

  eventSource.addEventListener("log", (ev) => {
    let log;
    try {
      log = JSON.parse(ev.data);
    } catch {
      return;
    }
    if (!matchesFilters(log)) return;

    if (stickToTop || isNearTop()) {
      prependLog(log);
    } else {
      pendingNew.push(log);
      updateBanner();
    }
  });

  eventSource.onerror = () => {
    // browser will retry; keep button state
  };
}

liveBtn.addEventListener("click", () => {
  if (eventSource) stopLive();
  else startLive();
});

searchBtn.addEventListener("click", () => loadLogs().catch(console.error));
searchInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") loadLogs().catch(console.error);
});
appFilter.addEventListener("change", () => loadLogs().catch(console.error));
levelFilter.addEventListener("change", () => loadLogs().catch(console.error));

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
  navUser.textContent = data.username || "admin";
  const badge = document.getElementById("nav-badge");
  if (data.auth_mode === "anonymous" && badge) {
    badge.hidden = false;
    badge.textContent = "anonymous";
    logoutBtn.hidden = true;
  }
  await loadApps();
  await loadLogs();
  startLive();
}

boot().catch(() => {
  window.location.href = "/login";
});

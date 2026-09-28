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
const pickAll = document.getElementById("pick-all");
const selectBar = document.getElementById("select-bar");
const selectCount = document.getElementById("select-count");
const toastEl = document.getElementById("toast");

// Picked rows by id, in the order they were picked. Cleared when the list reloads.
const picked = new Map();
let detailLog = null;

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

  const tdPick = document.createElement("td");
  tdPick.className = "pick";
  const box = document.createElement("input");
  box.type = "checkbox";
  box.setAttribute("aria-label", "Select row");
  box.checked = picked.has(log.id);
  tr.classList.toggle("picked", box.checked);
  box.addEventListener("change", () => setPicked(log, tr, box.checked));
  tdPick.appendChild(box);

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

  tr.append(tdPick, tdTime, tdApp, tdLevel, tdMsg);
  tr.addEventListener("click", (e) => {
    if (e.target.closest(".pick")) return;
    // Dragging over text to copy it must not pop the dialog open.
    const sel = window.getSelection();
    if (sel && !sel.isCollapsed && sel.toString().trim()) return;
    showDetail(log);
  });
  return tr;
}

function setPicked(log, tr, on) {
  if (on) picked.set(log.id, log);
  else picked.delete(log.id);
  tr.classList.toggle("picked", on);
  updateSelectBar();
}

function updateSelectBar() {
  const n = picked.size;
  selectBar.hidden = n === 0;
  selectCount.textContent = `${n} selected`;
  const boxes = rowsEl.querySelectorAll(".pick input");
  const checked = rowsEl.querySelectorAll(".pick input:checked").length;
  pickAll.checked = boxes.length > 0 && checked === boxes.length;
  pickAll.indeterminate = checked > 0 && checked < boxes.length;
}

function clearPicked() {
  picked.clear();
  for (const tr of rowsEl.querySelectorAll("tr.picked")) {
    tr.classList.remove("picked");
    tr.querySelector(".pick input").checked = false;
  }
  updateSelectBar();
}

// One line per log, oldest first, ready to paste into a chat or a ticket.
function logAsText(log) {
  const src = log.source ? ` (${log.source})` : "";
  const meta = log.meta ? ` ${JSON.stringify(log.meta)}` : "";
  return `${log.timestamp} ${String(log.level || "").toUpperCase()} ${log.app}${src}: ${log.message}${meta}`;
}

function pickedLogs() {
  return [...picked.values()].sort((a, b) =>
    a.timestamp === b.timestamp ? a.id - b.id : a.timestamp < b.timestamp ? -1 : 1
  );
}

let toastTimer = null;
function toast(msg, bad = false) {
  toastEl.textContent = msg;
  toastEl.classList.toggle("bad", bad);
  toastEl.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (toastEl.hidden = true), 1800);
}

// navigator.clipboard needs HTTPS or localhost; plain-HTTP deployments fall back to execCommand.
async function copyText(text, label) {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
    } else {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.setAttribute("readonly", "");
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      (dialog.open ? dialog : document.body).appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      ta.remove();
      if (!ok) throw new Error("copy refused");
    }
    toast(`Copied ${label}`);
  } catch {
    toast("Copy failed — select the text and press Ctrl/Cmd+C", true);
  }
}

pickAll.addEventListener("change", () => {
  for (const tr of rowsEl.querySelectorAll("tr")) {
    const box = tr.querySelector(".pick input");
    if (!box || box.checked === pickAll.checked) continue;
    box.checked = pickAll.checked;
    box.dispatchEvent(new Event("change"));
  }
});

document.getElementById("copy-text-btn").addEventListener("click", () => {
  const logs = pickedLogs();
  copyText(logs.map(logAsText).join("\n"), `${logs.length} line${logs.length === 1 ? "" : "s"}`);
});
document.getElementById("copy-json-btn").addEventListener("click", () => {
  const logs = pickedLogs();
  copyText(JSON.stringify(logs, null, 2), `${logs.length} as JSON`);
});
document.getElementById("select-clear-btn").addEventListener("click", clearPicked);

dialog.addEventListener("click", (e) => {
  const kind = e.target.closest("[data-copy]")?.dataset.copy;
  if (!kind || !detailLog) return;
  if (kind === "message") copyText(detailLog.message || "", "message");
  else if (kind === "meta") copyText(detailLog.meta ? JSON.stringify(detailLog.meta, null, 2) : "", "meta");
  else copyText(JSON.stringify(detailLog, null, 2), "JSON");
});

function prependLog(log) {
  const existing = rowsEl.querySelector(`[data-id="${log.id}"]`);
  if (existing) return;
  rowsEl.prepend(createRow(log));
  if (picked.size) updateSelectBar();
}

function appendLogs(logs) {
  const frag = document.createDocumentFragment();
  for (const log of logs) {
    frag.appendChild(createRow(log));
  }
  rowsEl.appendChild(frag);
}

function showDetail(log) {
  detailLog = log;
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

// Export takes the list filters but no limit: the server streams every matching row.
function exportUrl() {
  const params = new URLSearchParams(queryString());
  params.delete("limit");
  params.set("format", document.getElementById("export-format").value);
  return `/api/v1/logs/export?${params}`;
}

document.getElementById("export-btn").addEventListener("click", () => {
  const a = document.createElement("a");
  a.href = exportUrl();
  a.download = "";
  document.body.appendChild(a);
  a.click();
  a.remove();
  toast("Export started");
});

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
  picked.clear();
  appendLogs(data.logs || []);
  updateSelectBar();
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

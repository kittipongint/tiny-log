const navUser = document.getElementById("nav-user");
const logoutBtn = document.getElementById("logout-btn");
const retentionInput = document.getElementById("retention-days");
const sessionInput = document.getElementById("session-days");
const saveBtn = document.getElementById("save-settings");
const settingsMsg = document.getElementById("settings-msg");
const settingsErr = document.getElementById("settings-err");
const changePasswordBtn = document.getElementById("change-password");
const passwordMsg = document.getElementById("password-msg");
const passwordErr = document.getElementById("password-err");
const runRetentionBtn = document.getElementById("run-retention");
const retentionMsg = document.getElementById("retention-msg");
const confirmDialog = document.getElementById("confirm-dialog");
const confirmText = document.getElementById("confirm-text");

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

function show(el, text) {
  el.textContent = text;
  el.hidden = false;
}

function hide(...els) {
  for (const el of els) el.hidden = true;
}

logoutBtn.addEventListener("click", async () => {
  await api("/api/auth/logout", { method: "POST" });
  window.location.href = "/login";
});

saveBtn.addEventListener("click", async () => {
  hide(settingsMsg, settingsErr);
  const retention_days = Number(retentionInput.value);
  const session_days = Number(sessionInput.value);
  const res = await api("/api/v1/admin/settings", {
    method: "PUT",
    body: JSON.stringify({ retention_days, session_days }),
  });
  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    show(settingsErr, data.error || "Failed to save settings");
    return;
  }
  const data = await res.json();
  retentionInput.value = data.retention_days;
  sessionInput.value = data.session_days;
  show(settingsMsg, "Settings saved.");
});

changePasswordBtn.addEventListener("click", async () => {
  hide(passwordMsg, passwordErr);
  const current_password = document.getElementById("current-password").value;
  const new_password = document.getElementById("new-password").value;
  const confirm_password = document.getElementById("confirm-password").value;

  const res = await api("/api/v1/admin/password", {
    method: "POST",
    body: JSON.stringify({ current_password, new_password, confirm_password }),
  });

  if (!res.ok) {
    const data = await res.json().catch(() => ({}));
    show(passwordErr, data.error || "Password change failed");
    return;
  }

  show(passwordMsg, "Password changed. Please log in again.");
  setTimeout(() => {
    window.location.href = "/login";
  }, 800);
});

function confirmDelete(message) {
  confirmText.textContent = message;
  confirmDialog.showModal();
  return new Promise((resolve) => {
    confirmDialog.addEventListener(
      "close",
      () => resolve(confirmDialog.returnValue === "confirm"),
      { once: true }
    );
  });
}

runRetentionBtn.addEventListener("click", async () => {
  hide(retentionMsg);
  const days = retentionInput.value || "?";
  const ok = await confirmDelete(`Delete all logs older than ${days} days?`);
  if (!ok) return;

  const res = await api("/api/v1/admin/retention/run", { method: "POST" });
  if (!res.ok) {
    show(retentionMsg, "Cleanup failed.");
    retentionMsg.className = "error";
    retentionMsg.hidden = false;
    return;
  }
  const data = await res.json();
  retentionMsg.className = "ok";
  show(retentionMsg, `Deleted ${data.deleted} log(s).`);
});

async function boot() {
  const me = await api("/api/auth/me");
  if (!me.ok) return;
  const meData = await me.json();
  navUser.textContent = meData.username || "admin";

  const res = await api("/api/v1/admin/settings");
  if (!res.ok) throw new Error("settings");
  const data = await res.json();
  retentionInput.value = data.retention_days;
  sessionInput.value = data.session_days;
}

boot().catch(() => {
  window.location.href = "/login";
});

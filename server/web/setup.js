const form = document.getElementById("setup-form");
const errorEl = document.getElementById("setup-error");
const tokenEl = document.getElementById("setup-token");

// The start-up log prints /setup#token=…; the fragment never reaches the server or proxy logs.
const hashToken = new URLSearchParams(window.location.hash.slice(1)).get("token");
if (hashToken) {
  tokenEl.value = hashToken;
  history.replaceState(null, "", window.location.pathname);
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  errorEl.hidden = true;

  const setup_token = tokenEl.value.trim();
  const username = document.getElementById("username").value.trim();
  const password = document.getElementById("password").value;
  const confirm_password = document.getElementById("confirm").value;

  try {
    const res = await fetch("/api/auth/setup", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      credentials: "same-origin",
      body: JSON.stringify({ setup_token, username, password, confirm_password }),
    });
    if (!res.ok) {
      const data = await res.json().catch(() => ({}));
      errorEl.textContent =
        res.status === 401 ? "Setup token is wrong" : data.error || "Setup failed";
      errorEl.hidden = false;
      return;
    }
    window.location.href = "/";
  } catch {
    errorEl.textContent = "Setup failed";
    errorEl.hidden = false;
  }
});

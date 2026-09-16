const form = document.getElementById("setup-form");
const errorEl = document.getElementById("setup-error");

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  errorEl.hidden = true;

  const username = document.getElementById("username").value.trim();
  const password = document.getElementById("password").value;
  const confirm_password = document.getElementById("confirm").value;

  try {
    const res = await fetch("/api/auth/setup", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      credentials: "same-origin",
      body: JSON.stringify({ username, password, confirm_password }),
    });
    if (!res.ok) {
      const data = await res.json().catch(() => ({}));
      errorEl.textContent = data.error || "Setup failed";
      errorEl.hidden = false;
      return;
    }
    window.location.href = "/";
  } catch {
    errorEl.textContent = "Setup failed";
    errorEl.hidden = false;
  }
});

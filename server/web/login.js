async function boot() {
  try {
    const res = await fetch("/api/auth/me", { credentials: "same-origin" });
    if (res.ok) {
      const data = await res.json();
      if (data.setup_required) {
        window.location.href = "/setup";
        return;
      }
      if (data.authenticated || data.auth_mode === "anonymous") {
        window.location.href = "/";
      }
    }
  } catch {
    // stay on login
  }
}
boot();

const form = document.getElementById("login-form");
const errorEl = document.getElementById("login-error");

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  errorEl.hidden = true;

  const username = document.getElementById("username").value.trim();
  const password = document.getElementById("password").value;

  try {
    const res = await fetch("/api/auth/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      credentials: "same-origin",
      body: JSON.stringify({ username, password }),
    });

    if (!res.ok) {
      errorEl.textContent =
        res.status === 429
          ? "Too many attempts. Try again later."
          : "Invalid username or password.";
      errorEl.hidden = false;
      return;
    }

    window.location.href = "/";
  } catch {
    errorEl.textContent = "Login failed.";
    errorEl.hidden = false;
  }
});

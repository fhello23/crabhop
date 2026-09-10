// Basic Auth has no application session to revoke. Attempt to replace the
// browser's cached credentials, then independently check protected access.
// A rejected bogus password alone does not prove the real one was forgotten.
document.addEventListener("DOMContentLoaded", () => {
  const btn = document.getElementById("logout-button");
  const status = document.getElementById("logout-status");
  if (!btn) return;
  const say = (msg) => {
    if (status) status.textContent = msg;
  };
  const timeoutMs = 10_000;
  function forgetCredentials() {
    return new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest();
      xhr.open("GET", "/admin", true, "logged-out", `logged-out-${Date.now()}`);
      xhr.timeout = timeoutMs;
      xhr.onload = () => xhr.status === 401 ? resolve() : reject(new Error("Unexpected logout response"));
      xhr.onerror = xhr.onabort = xhr.ontimeout = () => reject(new Error("Logout request failed"));
      xhr.send();
    });
  }

  btn.addEventListener("click", async () => {
    if (btn.disabled) return;
    btn.disabled = true;
    say("Logging out…");
    let loggedOut = false;
    try {
      await forgetCredentials();
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), timeoutMs);
      try {
        // Send only credentials the browser would normally use. Do not omit
        // them or supply the bogus password: either would hide cached access.
        const response = await fetch("/admin", {
          credentials: "same-origin",
          cache: "no-store",
          redirect: "error",
          signal: controller.signal,
        });
        if (response.status !== 401) throw new Error("Logout could not be verified");
      } finally {
        clearTimeout(timeout);
      }
      loggedOut = true;
      say("Logged out. Sign in again to access admin.");
      const link = document.getElementById("logout-admin-link");
      if (link) link.textContent = "Sign in";
    } catch {
      say("Could not confirm logout. You may still be signed in. Try again, or quit your browser completely.");
    } finally {
      btn.disabled = loggedOut;
    }
  });
});

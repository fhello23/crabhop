// Progressive enhancement: copy short URLs and read-only shared text.
// Supports both input-based buttons (data-copy-target="<input id>") and
// direct-value buttons (data-copy-value="<text>").
// No inline scripts are used so the admin CSP (script-src 'self') holds.
document.addEventListener("click", async (event) => {
  const btn = event.target.closest("[data-copy-target],[data-copy-value]");
  if (!btn) return;
  let text = null;
  let input = null;
  if (btn.hasAttribute("data-copy-value")) {
    text = btn.getAttribute("data-copy-value");
  } else {
    input = document.getElementById(btn.getAttribute("data-copy-target"));
    if (!input || !("value" in input)) return;
    text = input.value;
  }
  if (text === null) return;
  const flash = () => {
    const status = document.getElementById("copy-status");
    if (status) status.textContent = "Text copied.";
    const original = btn.textContent;
    btn.textContent = "Copied!";
    setTimeout(() => { btn.textContent = original; }, 1500);
  };
  try {
    await navigator.clipboard.writeText(text);
    flash();
  } catch {
    let copied = false;
    if (input) {
      input.dispatchEvent(new Event("reveal-text"));
      input.focus();
      input.select();
      try { copied = document.execCommand("copy"); } catch { /* manual copy remains available */ }
    }
    if (copied) {
      flash();
    } else {
      const status = document.getElementById("copy-status");
      if (status) status.textContent = "Text selected. Use your device’s copy command to copy it.";
    }
  }
});

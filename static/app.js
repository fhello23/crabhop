// Progressive enhancement: copy-to-clipboard for short URLs.
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
    const original = btn.textContent;
    btn.textContent = "Copied!";
    setTimeout(() => { btn.textContent = original; }, 1500);
  };
  try {
    await navigator.clipboard.writeText(text);
    flash();
  } catch {
    if (input) {
      input.select();
      document.execCommand("copy");
      flash();
    }
  }
});

// Progressive enhancement: copy-to-clipboard for short URLs.
// No inline scripts are used so the admin CSP (script-src 'self') holds.
document.addEventListener("click", async (event) => {
  const btn = event.target.closest("[data-copy-target]");
  if (!btn) return;
  const input = document.getElementById(btn.getAttribute("data-copy-target"));
  if (!input || !("value" in input)) return;
  const text = input.value;
  try {
    await navigator.clipboard.writeText(text);
    const original = btn.textContent;
    btn.textContent = "Copied!";
    setTimeout(() => { btn.textContent = original; }, 1500);
  } catch {
    input.select();
    document.execCommand("copy");
  }
});

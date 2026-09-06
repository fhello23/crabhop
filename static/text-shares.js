// Previews never change the stored text. All rendering runs locally, with raw
// HTML disabled and images rendered as alt text so previews make no requests.
(() => {
  if (!window.markdownit || !window.hljs) return;
  const md = window.markdownit({
    html: false,
    highlight(source, language) {
      if (!language || !hljs.getLanguage(language)) return "";
      try {
        return hljs.highlight(source, { language, ignoreIllegals: true }).value;
      } catch {
        return ""; // markdown-it escapes code when highlighting is unavailable.
      }
    },
  });
  const validateLink = md.validateLink.bind(md);
  md.validateLink = (href) => {
    try {
      return validateLink(href) && ["http:", "https:", "mailto:"].includes(new URL(href, location.origin).protocol);
    } catch {
      return false;
    }
  };
  md.renderer.rules.image = (tokens, index, options, env, renderer) =>
    md.utils.escapeHtml(renderer.renderInlineAsText(tokens[index].children || [], options, env));
  md.renderer.rules.link_open = (tokens, index, options, env, renderer) => {
    tokens[index].attrSet("rel", "nofollow noopener noreferrer");
    return renderer.renderToken(tokens, index, options);
  };
  // Table alignment uses classes, keeping the existing CSP free of inline CSS.
  for (const rule of ["th_open", "td_open"]) {
    md.renderer.rules[rule] = (tokens, index, options, env, renderer) => {
      const token = tokens[index];
      const style = token.attrGet("style");
      if (style) {
        token.attrs = token.attrs.filter(([name]) => name !== "style");
        const alignment = /^text-align:(left|center|right)$/.exec(style);
        if (alignment) token.attrSet("class", `align-${alignment[1]}`);
      }
      return renderer.renderToken(tokens, index, options);
    };
  }

  document.querySelectorAll("[data-text-share]").forEach((container, index) => {
    const source = container.querySelector("textarea");
    if (!source) return;
    const sourceLabel = container.querySelector("[data-text-label]");
    const toolbar = document.createElement("div");
    toolbar.className = "actions text-views";
    toolbar.setAttribute("role", "group");
    toolbar.setAttribute("aria-label", "Text view");
    const preview = document.createElement("div");
    preview.id = `text-preview-${index}`;
    preview.className = "text-preview";
    preview.setAttribute("role", "region");
    preview.setAttribute("aria-label", "Text preview");
    preview.tabIndex = 0;
    preview.hidden = true;

    const languageLabel = document.createElement("label");
    languageLabel.className = "code-language";
    languageLabel.textContent = "Code language ";
    const language = document.createElement("select");
    language.add(new Option("Auto detect", "auto"));
    for (const name of hljs.listLanguages().sort()) {
      language.add(new Option(hljs.getLanguage(name).name || name, name));
    }
    languageLabel.append(language);
    languageLabel.hidden = true;

    let view = "plain";
    let pending;
    const buttons = new Map();
    function render() {
      clearTimeout(pending);
      preview.hidden = view === "plain";
      languageLabel.hidden = view !== "code";
      // Keep the editor available while previewing a draft.
      if (source.readOnly) {
        source.hidden = view !== "plain";
        if (sourceLabel) sourceLabel.hidden = view !== "plain";
      }
      for (const [name, button] of buttons) button.setAttribute("aria-pressed", String(name === view));
      if (view === "plain") return;
      if (view === "markdown") {
        preview.innerHTML = md.render(source.value);
      } else {
        const pre = document.createElement("pre");
        const code = document.createElement("code");
        code.className = "hljs";
        code.textContent = source.value;
        try {
          if (language.value !== "auto") {
            code.innerHTML = hljs.highlight(source.value, { language: language.value, ignoreIllegals: true }).value;
          } else if (source.value.length <= 16384) {
            // Bound automatic detection on long pastes. Explicit languages
            // remain available for the full 64 KiB text allowance.
            code.innerHTML = hljs.highlightAuto(source.value, ["javascript", "typescript", "python", "rust", "json", "bash", "sql", "xml"]).value;
          }
        } catch {
          code.textContent = source.value;
        }
        pre.append(code);
        preview.replaceChildren(pre);
      }
    }
    for (const [name, label] of [["plain", "Plain text"], ["markdown", "Markdown preview"], ["code", "Code"]]) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "secondary small";
      button.textContent = label;
      button.setAttribute("aria-controls", preview.id);
      button.addEventListener("click", () => { view = name; render(); });
      buttons.set(name, button);
      toolbar.append(button);
    }
    container.prepend(toolbar);
    container.append(languageLabel, preview);
    language.addEventListener("change", render);
    source.addEventListener("input", () => {
      clearTimeout(pending);
      if (view !== "plain") pending = setTimeout(render, 150);
    });
    source.addEventListener("reveal-text", () => { view = "plain"; render(); });
    render();
  });
})();

// Local presentation with explicit UTC submission. Without JavaScript the
// original datetime-local field remains named and clearly labeled as UTC.
(() => {
  const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "long" });
  const dateOnly = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
  document.querySelectorAll("time[data-local-time]").forEach(element => {
    const date = new Date(element.dateTime);
    if (!Number.isFinite(date.getTime())) return;
    element.textContent = (element.dataset.localTime === "date" ? dateOnly : dateTime).format(date);
    element.title = `${dateTime.format(date)} (${timezone})`;
  });

  const pad = (value, length = 2) => String(value).padStart(length, "0");
  const localValue = date => `${pad(date.getFullYear(), 4)}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}.${pad(date.getMilliseconds(), 3)}`;
  function localDate(value) {
    const parts = /^(\d{4,})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,3}))?)?$/.exec(value);
    if (!parts) return null;
    const [year, month, day, hour, minute, second, millis] = [
      Number(parts[1]), Number(parts[2]) - 1, Number(parts[3]), Number(parts[4]), Number(parts[5]),
      Number(parts[6] || 0), Number((parts[7] || "").padEnd(3, "0")),
    ];
    const date = new Date(0);
    date.setFullYear(year, month, day);
    date.setHours(hour, minute, second, millis);
    // Reject nonexistent local times during a daylight-saving jump, instead
    // of silently moving the requested expiry forward by an hour.
    return date.getFullYear() === year && date.getMonth() === month && date.getDate() === day &&
      date.getHours() === hour && date.getMinutes() === minute && date.getSeconds() === second &&
      date.getMilliseconds() === millis ? date : null;
  }

  document.querySelectorAll("[data-expiration]").forEach(container => {
    const input = container.querySelector("[data-expiration-input]");
    const form = input.form;
    const help = container.querySelector("[data-expiration-help]");
    const initialInstant = input.dataset.instant;
    if (initialInstant) input.value = localValue(new Date(initialInstant));
    const initialDisplay = input.value;
    let lastDisplay = initialDisplay;
    let lastInstant = initialInstant;
    input.defaultValue = initialDisplay;
    const absolute = document.createElement("input");
    absolute.type = "hidden";
    absolute.name = input.name;
    absolute.value = initialInstant;
    input.removeAttribute("name");
    container.append(absolute);
    container.querySelector("[data-expiration-label]").textContent = `Expires (${timezone}, optional)`;

    function sync() {
      input.setCustomValidity("");
      if (!input.value) {
        absolute.value = "";
        lastDisplay = "";
        lastInstant = "";
        help.textContent = "No expiration. Presets start from now.";
        return;
      }
      // Preserve the exact original instant (including the later occurrence
      // of a repeated DST hour) when saving an unrelated edit.
      const date = input.value === lastDisplay && lastInstant ? new Date(lastInstant) : localDate(input.value);
      if (!date || !Number.isFinite(date.getTime())) {
        absolute.value = "";
        input.setCustomValidity("This local time does not exist. Choose a valid time in your timezone.");
        help.textContent = input.validationMessage;
        return;
      }
      absolute.value = date.toISOString();
      lastDisplay = input.value;
      lastInstant = absolute.value;
      help.textContent = `Expires ${dateTime.format(date)}. Leave empty for no expiration.`;
    }
    const presets = document.createElement("div");
    presets.className = "actions expiration-presets";
    presets.setAttribute("role", "group");
    presets.setAttribute("aria-label", "Expiration presets");
    for (const [label, hours] of [["1 hour", 1], ["24 hours", 24], ["7 days", 168], ["Never", null]]) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "secondary small";
      button.textContent = label;
      button.addEventListener("click", () => {
        const date = hours === null ? null : new Date(Date.now() + hours * 3600000);
        input.value = date ? localValue(date) : "";
        // A preset may land in the later occurrence of a repeated local hour.
        // Keep its elapsed-time instant, which cannot be inferred from the field.
        lastDisplay = input.value;
        lastInstant = date ? date.toISOString() : "";
        sync();
      });
      presets.append(button);
    }
    input.after(presets);
    input.addEventListener("input", sync);
    input.addEventListener("change", sync);
    // Do not recalculate here: an untouched preset or repeated local hour must
    // retain the exact instant already recorded by the input/preset handlers.
    form.addEventListener("submit", event => {
      if (!input.checkValidity()) {
        event.preventDefault();
        input.reportValidity();
      }
    });
    form.addEventListener("reset", () => setTimeout(() => {
      lastDisplay = initialDisplay;
      lastInstant = initialInstant;
      sync();
    }, 0));
    sync();
  });
})();

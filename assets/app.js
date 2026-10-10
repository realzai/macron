// macron's web UI works without this script. It adds what has to be live: the schedule described
// as you type it, the jobs list keeping itself current, and a running job's output arriving as it
// is written. Everything it puts on the page goes in as text, never as markup it did not get from
// macron's own templates.
"use strict";

document.addEventListener("DOMContentLoaded", () => {
  describeSchedule();
  refreshJobs();
  followRun();
});

/** Describes the schedule being typed, and says what is wrong with it. */
function describeSchedule() {
  const input = document.querySelector("[data-describe]");
  const hint = document.querySelector("[data-description]");
  if (!input || !hint) return;
  const guidance = hint.innerHTML;
  let pending;
  input.addEventListener("input", () => {
    clearTimeout(pending);
    pending = setTimeout(async () => {
      const text = input.value.trim();
      if (!text) {
        hint.innerHTML = guidance;
        hint.classList.remove("invalid");
        return;
      }
      try {
        const response = await fetch("/schedule?text=" + encodeURIComponent(text));
        if (!response.ok || input.value.trim() !== text) return;
        const described = await response.json();
        if (described.error) {
          hint.textContent = described.error;
          hint.classList.add("invalid");
          return;
        }
        const when = described.description[0].toUpperCase() + described.description.slice(1);
        const next = described.next_runs.length ? ` Next: ${described.next_runs.join(", ")}.` : "";
        hint.textContent = `${when}.${next}`;
        hint.classList.remove("invalid");
      } catch {
        // Only a convenience: the form checks the schedule when it is sent.
      }
    }, 200);
  });
}

/** Keeps the jobs list current while the page is in view. */
function refreshJobs() {
  const jobs = document.querySelector("[data-refresh]");
  if (!jobs) return;
  setInterval(async () => {
    if (document.visibilityState !== "visible") return;
    try {
      const response = await fetch(jobs.dataset.refresh);
      const isPage = response.headers.get("content-type")?.startsWith("text/html");
      if (response.ok && isPage) jobs.innerHTML = await response.text();
    } catch {
      // The server may have stopped; the next tick tries again.
    }
  }, 5000);
}

/** Adds a running job's output to the page as it is written, and reloads when the run ends. */
function followRun() {
  const log = document.querySelector("[data-events]");
  if (!log) return;
  const events = new EventSource(log.dataset.events);
  events.addEventListener("line", (event) => {
    const line = JSON.parse(event.data);
    const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
    const row = document.createElement("div");
    row.className = `line ${line.stream}`;
    const time = document.createElement("span");
    time.className = "time";
    time.textContent = line.time;
    const text = document.createElement("span");
    text.className = "text";
    text.textContent = line.text;
    row.append(time, text);
    log.append(row);
    if (atBottom) log.scrollTop = log.scrollHeight;
  });
  events.addEventListener("end", () => {
    events.close();
    location.reload();
  });
  // A reconnecting stream would start again from where the page began, repeating lines, so a
  // dropped connection reloads the page instead, which picks up where the run has got to.
  events.addEventListener("error", () => {
    events.close();
    setTimeout(() => location.reload(), 2000);
  });
}

const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const activeMeta = document.querySelector("#activeMeta");
const activeDuration = document.querySelector("#activeDuration");
const stopButton = document.querySelector("#stopButton");
const timerForm = document.querySelector("#timerForm");
const taskName = document.querySelector("#taskName");
const githubKind = document.querySelector("#githubKind");
const githubReference = document.querySelector("#githubReference");
const githubTokenLabel = document.querySelector("#githubTokenLabel");
const githubToken = document.querySelector("#githubToken");
const githubSearchStatus = document.querySelector("#githubSearchStatus");
const githubSearchResults = document.querySelector("#githubSearchResults");
const subtaskName = document.querySelector("#subtaskName");
const note = document.querySelector("#note");
const taskNames = document.querySelector("#taskNames");
const recentRows = document.querySelector("#recentRows");
const summaryRows = document.querySelector("#summaryRows");

const LEGACY_GITHUB_TOKEN_STORAGE_KEY = "tracker.githubToken";

let activeTimer = null;
let tickHandle = null;
let githubSearchHandle = null;
let githubSearchRequest = 0;
let githubTokenSaveHandle = null;

function formatDuration(seconds) {
  const value = Math.max(0, Math.floor(seconds));
  const hours = String(Math.floor(value / 3600)).padStart(2, "0");
  const minutes = String(Math.floor((value % 3600) / 60)).padStart(2, "0");
  const secs = String(value % 60).padStart(2, "0");
  return `${hours}:${minutes}:${secs}`;
}

function formatDate(value) {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}

function referenceLabel(item) {
  if (!item.githubReference) return "";
  const kind = item.githubKind === "pull_request" ? "PR" : item.githubKind === "issue" ? "Issue" : "";
  return kind ? `${kind} ${item.githubReference}` : item.githubReference;
}

function renderActiveTimer() {
  window.clearInterval(tickHandle);

  if (!activeTimer) {
    activeMeta.textContent = "No timer running";
    activeDuration.textContent = "00:00:00";
    stopButton.disabled = true;
    return;
  }

  const startedAt = new Date(activeTimer.startedAt).getTime();
  const label = activeTimer.subtask
    ? `${activeTimer.task.name} / ${activeTimer.subtask.name}`
    : activeTimer.task.name;

  activeMeta.textContent = label;
  stopButton.disabled = false;

  const update = () => {
    activeDuration.textContent = formatDuration((Date.now() - startedAt) / 1000);
  };
  update();
  tickHandle = window.setInterval(update, 1000);
}

function renderTaskNames(tasks) {
  taskNames.innerHTML = "";
  for (const { task } of tasks) {
    const option = document.createElement("option");
    option.value = task.name;
    taskNames.append(option);
  }
}

function renderRecent(entries) {
  recentRows.innerHTML = "";
  if (!entries.length) {
    recentRows.innerHTML = `<tr><td colspan="5" class="muted">No time entries yet.</td></tr>`;
    return;
  }

  for (const entry of entries) {
    const row = document.createElement("tr");
    row.innerHTML = `
      <td>${escapeHtml(entry.taskName)}</td>
      <td class="muted">${escapeHtml(entry.subtaskName ?? "")}</td>
      <td class="muted">${escapeHtml(referenceLabel(entry))}</td>
      <td>${formatDate(entry.startedAt)}</td>
      <td class="mono">${formatDuration(entry.durationSeconds)}</td>
    `;
    recentRows.append(row);
  }
}

function renderSummary(rows) {
  summaryRows.innerHTML = "";
  if (!rows.length) {
    summaryRows.innerHTML = `<tr><td colspan="5" class="muted">No report data yet.</td></tr>`;
    return;
  }

  for (const item of rows) {
    const row = document.createElement("tr");
    row.innerHTML = `
      <td>${escapeHtml(item.taskName)}</td>
      <td class="muted">${escapeHtml(item.subtaskName ?? "")}</td>
      <td class="muted">${escapeHtml(referenceLabel(item))}</td>
      <td class="mono">${item.entryCount}</td>
      <td class="mono">${formatDuration(item.totalSeconds)}</td>
    `;
    summaryRows.append(row);
  }
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function isGithubReferenceKind() {
  return githubKind.value === "issue" || githubKind.value === "pull_request";
}

function updateGithubControls() {
  githubTokenLabel.classList.toggle("visible", isGithubReferenceKind());
  if (isGithubReferenceKind()) {
    scheduleGithubSearch();
    return;
  }

  githubSearchStatus.textContent = "";
  githubSearchResults.hidden = true;
  githubSearchResults.innerHTML = "";
}

function scheduleGithubSearch() {
  window.clearTimeout(githubSearchHandle);
  const query = githubReference.value.trim();

  if (!isGithubReferenceKind() || query.length < 3) {
    githubSearchRequest += 1;
    githubSearchStatus.textContent = "";
    githubSearchResults.hidden = true;
    githubSearchResults.innerHTML = "";
    return;
  }

  githubSearchHandle = window.setTimeout(searchGithubReferences, 350);
}

async function searchGithubReferences() {
  const requestId = ++githubSearchRequest;
  const query = githubReference.value.trim();

  githubSearchStatus.textContent = "Searching GitHub...";

  try {
    const results = await invoke("search_github_references", {
      input: {
        query,
        githubKind: githubKind.value,
      },
    });

    if (requestId !== githubSearchRequest) return;
    renderGithubResults(results);
  } catch (error) {
    if (requestId !== githubSearchRequest) return;
    githubSearchResults.hidden = true;
    githubSearchResults.innerHTML = "";
    githubSearchStatus.textContent = String(error);
  }
}

function renderGithubResults(results) {
  githubSearchResults.innerHTML = "";

  if (!results.length) {
    githubSearchResults.hidden = true;
    githubSearchStatus.textContent = "No GitHub matches found.";
    return;
  }

  githubSearchStatus.textContent = "";
  githubSearchResults.hidden = false;

  for (const result of results) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "search-result";
    button.innerHTML = `
      <strong>${escapeHtml(result.title)}</strong>
      <span>${escapeHtml(result.reference)} / ${escapeHtml(result.state)}</span>
    `;
    button.addEventListener("click", () => {
      githubReference.value = result.reference;
      if (!taskName.value.trim()) taskName.value = result.title;
      githubSearchResults.hidden = true;
      githubSearchStatus.textContent = result.reference;
    });
    githubSearchResults.append(button);
  }
}

async function loadGithubToken() {
  const savedToken = await invoke("get_github_token");
  const legacyToken = window.localStorage.getItem(LEGACY_GITHUB_TOKEN_STORAGE_KEY);

  if (savedToken) {
    githubToken.value = savedToken;
  } else if (legacyToken) {
    githubToken.value = legacyToken;
    await saveGithubToken();
  }

  window.localStorage.removeItem(LEGACY_GITHUB_TOKEN_STORAGE_KEY);
}

function scheduleGithubTokenSave() {
  window.clearTimeout(githubTokenSaveHandle);
  githubSearchRequest += 1;
  githubSearchResults.hidden = true;
  githubSearchResults.innerHTML = "";
  githubSearchStatus.textContent = githubToken.value.trim()
    ? "Saving GitHub token..."
    : "Clearing GitHub token...";

  githubTokenSaveHandle = window.setTimeout(saveGithubToken, 450);
}

async function saveGithubToken() {
  try {
    await invoke("set_github_token", {
      input: {
        token: githubToken.value || null,
      },
    });

    githubSearchStatus.textContent = githubToken.value.trim()
      ? "GitHub token saved."
      : "GitHub token cleared.";
    scheduleGithubSearch();
  } catch (error) {
    githubSearchStatus.textContent = String(error);
  }
}

async function refresh() {
  const [tasks, active, entries, summary] = await Promise.all([
    invoke("list_tasks"),
    invoke("get_active_timer"),
    invoke("recent_entries", { limit: 50 }),
    invoke("summary_by_task"),
  ]);

  activeTimer = active;
  renderTaskNames(tasks);
  renderActiveTimer();
  renderRecent(entries);
  renderSummary(summary);
}

timerForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const task = {
    name: taskName.value,
    githubKind: githubKind.value || null,
    githubReference: githubReference.value || null,
  };

  activeTimer = await invoke("start_timer", {
    input: {
      task,
      subtaskName: subtaskName.value || null,
      note: note.value || null,
    },
  });

  note.value = "";
  githubSearchResults.hidden = true;
  githubSearchStatus.textContent = "";
  renderActiveTimer();
  await refresh();
});

stopButton.addEventListener("click", async () => {
  await invoke("stop_timer");
  await refresh();
});

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((item) => item.classList.remove("active"));
    document.querySelectorAll(".view").forEach((item) => item.classList.remove("active"));
    tab.classList.add("active");
    document.querySelector(`#${tab.dataset.tab}View`).classList.add("active");
  });
});

githubKind.addEventListener("change", updateGithubControls);
githubReference.addEventListener("input", scheduleGithubSearch);
githubToken.addEventListener("input", scheduleGithubTokenSave);

await listen("timer-updated", refresh);
await loadGithubToken();
updateGithubControls();
await refresh();

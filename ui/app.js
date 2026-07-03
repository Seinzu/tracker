const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const activeMeta = document.querySelector("#activeMeta");
const activeDuration = document.querySelector("#activeDuration");
const stopButton = document.querySelector("#stopButton");
const timerForm = document.querySelector("#timerForm");
const startTimerButton = document.querySelector("#startTimerButton");
const taskSelect = document.querySelector("#taskSelect");
const createTaskButton = document.querySelector("#createTaskButton");
const githubTokenDialog = document.querySelector("#githubTokenDialog");
const githubToken = document.querySelector("#githubToken");
const githubTokenClose = document.querySelector("#githubTokenClose");
const githubTokenClear = document.querySelector("#githubTokenClear");
const githubTokenSave = document.querySelector("#githubTokenSave");
const githubTokenStatus = document.querySelector("#githubTokenStatus");
const subtaskName = document.querySelector("#subtaskName");
const note = document.querySelector("#note");
const recentRows = document.querySelector("#recentRows");
const summaryRows = document.querySelector("#summaryRows");
const summarySubtaskHeader = document.querySelector("#summarySubtaskHeader");
const reportTaskMode = document.querySelector("#reportTaskMode");
const reportSubtaskMode = document.querySelector("#reportSubtaskMode");
const createTaskDialog = document.querySelector("#createTaskDialog");
const createTaskClose = document.querySelector("#createTaskClose");
const createTaskCancel = document.querySelector("#createTaskCancel");
const createTaskStatus = document.querySelector("#createTaskStatus");
const createFreeTextMode = document.querySelector("#createFreeTextMode");
const createIssueMode = document.querySelector("#createIssueMode");
const createPullRequestMode = document.querySelector("#createPullRequestMode");
const createFreeTextPanel = document.querySelector("#createFreeTextPanel");
const createImportPanel = document.querySelector("#createImportPanel");
const freeTaskName = document.querySelector("#freeTaskName");
const createFreeTask = document.querySelector("#createFreeTask");
const createImportSearch = document.querySelector("#createImportSearch");
const createImportResults = document.querySelector("#createImportResults");

const LEGACY_GITHUB_TOKEN_STORAGE_KEY = "tracker.githubToken";

let activeTimer = null;
let taskItems = [];
let pendingSelectedTaskId = null;
let tickHandle = null;
let createTaskMode = "free";
let createImportSearchHandle = null;
let createImportSearchRequest = 0;
let hasGithubToken = false;
let reportMode = "task";
let taskSummaryRows = [];
let subtaskSummaryRows = [];

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

function taskOptionLabel(item) {
  const reference = referenceLabel(item.task);
  return reference ? `${item.task.name} (${reference})` : item.task.name;
}

function renderTaskOptions(tasks) {
  const previousTaskId = pendingSelectedTaskId ?? taskSelect.value;
  taskItems = tasks;
  taskSelect.innerHTML = `<option value="">Select task</option>`;

  for (const item of tasks) {
    const option = document.createElement("option");
    option.value = String(item.task.id);
    option.textContent = taskOptionLabel(item);
    taskSelect.append(option);
  }

  if (previousTaskId && tasks.some((item) => String(item.task.id) === String(previousTaskId))) {
    taskSelect.value = String(previousTaskId);
  }

  pendingSelectedTaskId = null;
  updateSelectedTaskDetails();
}

function selectedTaskItem() {
  return taskItems.find((item) => String(item.task.id) === taskSelect.value) ?? null;
}

function subtasksForTask(taskId) {
  return taskItems.find((item) => item.task.id === taskId)?.subtasks ?? [];
}

function updateSelectedTaskDetails() {
  const selected = selectedTaskItem();
  startTimerButton.disabled = !selected;
}

function renderRecent(entries) {
  recentRows.innerHTML = "";
  if (!entries.length) {
    recentRows.innerHTML = `<tr><td colspan="5" class="muted">No time entries yet.</td></tr>`;
    return;
  }

  for (const entry of entries) {
    const row = document.createElement("tr");
    const subtaskCell = document.createElement("td");
    const subtaskEditor = document.createElement("div");
    const subtaskInput = document.createElement("input");
    const saveSubtask = document.createElement("button");
    const listId = `subtasks-${entry.id}`;

    subtaskCell.className = "subtask-edit-cell";
    subtaskEditor.className = "inline-edit";
    subtaskInput.type = "text";
    subtaskInput.value = entry.subtaskName ?? "";
    subtaskInput.placeholder = "No subtask";
    subtaskInput.setAttribute("aria-label", `Subtask for ${entry.taskName}`);
    subtaskInput.setAttribute("list", listId);
    saveSubtask.type = "button";
    saveSubtask.className = "secondary compact";
    saveSubtask.textContent = "Save";
    saveSubtask.disabled = true;

    const datalist = document.createElement("datalist");
    datalist.id = listId;
    for (const subtask of subtasksForTask(entry.taskId)) {
      const option = document.createElement("option");
      option.value = subtask.name;
      datalist.append(option);
    }

    const syncSaveState = () => {
      saveSubtask.disabled = subtaskInput.value.trim() === (entry.subtaskName ?? "");
    };
    const save = async () => {
      if (saveSubtask.disabled) return;
      saveSubtask.disabled = true;
      saveSubtask.textContent = "Saving";
      try {
        await updateEntrySubtask(entry.id, subtaskInput.value);
      } catch (error) {
        saveSubtask.textContent = "Retry";
        saveSubtask.disabled = false;
        console.error(error);
      }
    };

    subtaskInput.addEventListener("input", syncSaveState);
    subtaskInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        save();
      }
    });
    saveSubtask.addEventListener("click", save);
    subtaskEditor.append(subtaskInput, saveSubtask, datalist);
    subtaskCell.append(subtaskEditor);

    row.innerHTML = `
      <td>${escapeHtml(entry.taskName)}</td>
      <td class="muted">${escapeHtml(referenceLabel(entry))}</td>
      <td>${formatDate(entry.startedAt)}</td>
      <td class="mono">${formatDuration(entry.durationSeconds)}</td>
    `;
    row.children[0].after(subtaskCell);
    recentRows.append(row);
  }
}

function renderSummary() {
  const isDetailed = reportMode === "subtask";
  const rows = isDetailed ? subtaskSummaryRows : taskSummaryRows;
  summarySubtaskHeader.hidden = !isDetailed;
  reportTaskMode.classList.toggle("active", !isDetailed);
  reportSubtaskMode.classList.toggle("active", isDetailed);
  summaryRows.innerHTML = "";

  if (!rows.length) {
    summaryRows.innerHTML = `<tr><td colspan="${isDetailed ? 5 : 4}" class="muted">No report data yet.</td></tr>`;
    return;
  }

  for (const item of rows) {
    const row = document.createElement("tr");
    row.innerHTML = `
      <td>${escapeHtml(item.taskName)}</td>
      ${isDetailed ? `<td class="muted">${escapeHtml(item.subtaskName ?? "No subtask")}</td>` : ""}
      <td class="muted">${escapeHtml(referenceLabel(item))}</td>
      <td class="mono">${item.entryCount}</td>
      <td class="mono">${formatDuration(item.totalSeconds)}</td>
    `;
    summaryRows.append(row);
  }
}

function setReportMode(mode) {
  reportMode = mode;
  renderSummary();
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
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

  hasGithubToken = Boolean(githubToken.value.trim());
  updateCreateModeAvailability();
  window.localStorage.removeItem(LEGACY_GITHUB_TOKEN_STORAGE_KEY);
}

async function saveGithubToken() {
  try {
    await invoke("set_github_token", {
      input: {
        token: githubToken.value || null,
      },
    });

    githubTokenStatus.textContent = githubToken.value.trim()
      ? "GitHub token saved."
      : "GitHub token cleared.";
    hasGithubToken = Boolean(githubToken.value.trim());
    updateCreateModeAvailability();
  } catch (error) {
    githubTokenStatus.textContent = String(error);
  }
}

function updateCreateModeAvailability() {
  createIssueMode.disabled = !hasGithubToken;
  createPullRequestMode.disabled = !hasGithubToken;

  if (!hasGithubToken && createTaskMode !== "free") {
    setCreateTaskMode("free");
  }
}

async function openGithubTokenDialog() {
  await loadGithubToken();
  githubTokenStatus.textContent = "";
  githubTokenDialog.hidden = false;
  githubToken.focus();
  githubToken.select();
}

function closeGithubTokenDialog() {
  githubTokenDialog.hidden = true;
}

async function clearGithubToken() {
  githubToken.value = "";
  await saveGithubToken();
}

function setCreateTaskMode(mode) {
  if ((mode === "issue" || mode === "pull_request") && !hasGithubToken) {
    createTaskStatus.textContent = "Set a GitHub token from the Tracker menu before importing.";
    return;
  }

  createTaskMode = mode;
  createFreeTextMode.classList.toggle("active", mode === "free");
  createIssueMode.classList.toggle("active", mode === "issue");
  createPullRequestMode.classList.toggle("active", mode === "pull_request");
  createFreeTextPanel.classList.toggle("active", mode === "free");
  createImportPanel.classList.toggle("active", mode !== "free");
  createTaskStatus.textContent =
    mode === "free" && !hasGithubToken
      ? "GitHub imports are disabled until a token is saved from the Tracker menu."
      : "";
  createImportResults.hidden = true;
  createImportResults.innerHTML = "";

  if (mode === "free") {
    freeTaskName.focus();
  } else {
    createImportSearch.focus();
    scheduleCreateImportSearch();
  }
}

async function openCreateTaskDialog() {
  await loadGithubToken();
  freeTaskName.value = "";
  createImportSearch.value = "";
  createTaskStatus.textContent = hasGithubToken
    ? ""
    : "GitHub imports are disabled until a token is saved from the Tracker menu.";
  createTaskDialog.hidden = false;
  setCreateTaskMode("free");
}

function closeCreateTaskDialog() {
  createTaskDialog.hidden = true;
}

async function persistCreatedTask(task) {
  const created = await invoke("create_task", { input: task });
  pendingSelectedTaskId = created.task.id;
  await refresh();
  closeCreateTaskDialog();
}

async function updateEntrySubtask(entryId, subtaskName) {
  await invoke("update_time_entry_subtask", {
    input: {
      entryId,
      subtaskName: subtaskName.trim() || null,
    },
  });
  await refresh();
}

async function createFreeTextTask() {
  const name = freeTaskName.value.trim();
  if (!name) {
    createTaskStatus.textContent = "Task name is required.";
    return;
  }

  try {
    await persistCreatedTask({
      name,
      githubKind: null,
      githubReference: null,
    });
  } catch (error) {
    createTaskStatus.textContent = String(error);
  }
}

function scheduleCreateImportSearch() {
  window.clearTimeout(createImportSearchHandle);
  const query = createImportSearch.value.trim();

  if (createTaskMode === "free" || query.length < 3) {
    createImportSearchRequest += 1;
    createImportResults.hidden = true;
    createImportResults.innerHTML = "";
    if (createTaskMode !== "free") createTaskStatus.textContent = "";
    return;
  }

  createImportSearchHandle = window.setTimeout(searchCreateImportTasks, 350);
}

async function searchCreateImportTasks() {
  const requestId = ++createImportSearchRequest;
  createTaskStatus.textContent = "Searching GitHub...";

  try {
    const results = await invoke("search_github_references", {
      input: {
        query: createImportSearch.value.trim(),
        githubKind: createTaskMode,
      },
    });

    if (requestId !== createImportSearchRequest) return;
    renderCreateImportResults(results);
  } catch (error) {
    if (requestId !== createImportSearchRequest) return;
    createImportResults.hidden = true;
    createImportResults.innerHTML = "";
    createTaskStatus.textContent = String(error);
  }
}

function renderCreateImportResults(results) {
  createImportResults.innerHTML = "";

  if (!results.length) {
    createImportResults.hidden = true;
    createTaskStatus.textContent = "No GitHub matches found.";
    return;
  }

  createTaskStatus.textContent = "";
  createImportResults.hidden = false;

  for (const result of results) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "search-result";
    button.innerHTML = `
      <strong>${escapeHtml(result.title)}</strong>
      <span>${escapeHtml(result.reference)} / ${escapeHtml(result.state)}</span>
    `;
    button.addEventListener("click", async () => {
      try {
        await persistCreatedTask({
          name: result.title,
          githubKind: createTaskMode,
          githubReference: result.reference,
        });
      } catch (error) {
        createTaskStatus.textContent = String(error);
      }
    });
    createImportResults.append(button);
  }
}

async function refresh() {
  const [tasks, active, entries, taskSummary, subtaskSummary] = await Promise.all([
    invoke("list_tasks"),
    invoke("get_active_timer"),
    invoke("recent_entries", { limit: 50 }),
    invoke("summary_by_task"),
    invoke("summary_by_subtask"),
  ]);

  activeTimer = active;
  taskSummaryRows = taskSummary;
  subtaskSummaryRows = subtaskSummary;
  renderTaskOptions(tasks);
  renderActiveTimer();
  renderRecent(entries);
  renderSummary();
}

timerForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const selected = selectedTaskItem();
  if (!selected) {
    return;
  }

  const task = {
    name: selected.task.name,
    githubKind: selected.task.githubKind ?? null,
    githubReference: selected.task.githubReference ?? null,
  };

  activeTimer = await invoke("start_timer", {
    input: {
      task,
      subtaskName: subtaskName.value || null,
      note: note.value || null,
    },
  });

  note.value = "";
  renderActiveTimer();
  await refresh();
});

taskSelect.addEventListener("change", updateSelectedTaskDetails);
createTaskButton.addEventListener("click", openCreateTaskDialog);
reportTaskMode.addEventListener("click", () => setReportMode("task"));
reportSubtaskMode.addEventListener("click", () => setReportMode("subtask"));
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

createFreeTextMode.addEventListener("click", () => setCreateTaskMode("free"));
createIssueMode.addEventListener("click", () => setCreateTaskMode("issue"));
createPullRequestMode.addEventListener("click", () => setCreateTaskMode("pull_request"));
createFreeTask.addEventListener("click", createFreeTextTask);
freeTaskName.addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    createFreeTextTask();
  }
});
createImportSearch.addEventListener("input", scheduleCreateImportSearch);
createTaskClose.addEventListener("click", closeCreateTaskDialog);
createTaskCancel.addEventListener("click", closeCreateTaskDialog);
createTaskDialog.addEventListener("click", (event) => {
  if (event.target === createTaskDialog) closeCreateTaskDialog();
});
githubTokenSave.addEventListener("click", saveGithubToken);
githubTokenClear.addEventListener("click", clearGithubToken);
githubTokenClose.addEventListener("click", closeGithubTokenDialog);
githubTokenDialog.addEventListener("click", (event) => {
  if (event.target === githubTokenDialog) closeGithubTokenDialog();
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !createTaskDialog.hidden) closeCreateTaskDialog();
  if (event.key === "Escape" && !githubTokenDialog.hidden) closeGithubTokenDialog();
});

await listen("timer-updated", refresh);
await listen("open-github-token-settings", openGithubTokenDialog);
await loadGithubToken();
await refresh();

const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const activeMeta = document.querySelector("#activeMeta");
const activeDuration = document.querySelector("#activeDuration");
const stopButton = document.querySelector("#stopButton");
const timerForm = document.querySelector("#timerForm");
const startTimerButton = document.querySelector("#startTimerButton");
const taskSelect = document.querySelector("#taskSelect");
const createTaskButton = document.querySelector("#createTaskButton");
const closeTaskButton = document.querySelector("#closeTaskButton");
const taskSuggestions = document.querySelector("#taskSuggestions");
const githubTokenDialog = document.querySelector("#githubTokenDialog");
const githubToken = document.querySelector("#githubToken");
const githubTokenClose = document.querySelector("#githubTokenClose");
const githubTokenClear = document.querySelector("#githubTokenClear");
const githubTokenSave = document.querySelector("#githubTokenSave");
const githubTokenStatus = document.querySelector("#githubTokenStatus");
const subtaskName = document.querySelector("#subtaskName");
const subtaskSuggestions = document.querySelector("#subtaskSuggestions");
const note = document.querySelector("#note");
const recentRows = document.querySelector("#recentRows");
const summaryRows = document.querySelector("#summaryRows");
const summaryHeader = document.querySelector("#summaryHeader");
const subtaskRows = document.querySelector("#subtaskRows");
const newSubtaskName = document.querySelector("#newSubtaskName");
const addSubtaskButton = document.querySelector("#addSubtaskButton");
const subtaskStatus = document.querySelector("#subtaskStatus");
const databaseErrorDialog = document.querySelector("#databaseErrorDialog");
const databaseErrorMessage = document.querySelector("#databaseErrorMessage");
const databaseErrorBackup = document.querySelector("#databaseErrorBackup");
const databaseErrorClose = document.querySelector("#databaseErrorClose");
const reportAllTimePeriod = document.querySelector("#reportAllTimePeriod");
const reportTodayPeriod = document.querySelector("#reportTodayPeriod");
const reportThisWeekPeriod = document.querySelector("#reportThisWeekPeriod");
const reportLastWeekPeriod = document.querySelector("#reportLastWeekPeriod");
const reportThisMonthPeriod = document.querySelector("#reportThisMonthPeriod");
const reportTotalDuration = document.querySelector("#reportTotalDuration");
const reportTaskMode = document.querySelector("#reportTaskMode");
const reportSubtaskMode = document.querySelector("#reportSubtaskMode");
const reportTaskSubtaskMode = document.querySelector("#reportTaskSubtaskMode");
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

/// Column headings for each report grouping, in render order.
const REPORT_COLUMNS = {
  task: ["Task", "Reference", "Entries", "Total"],
  subtask: ["Subtask", "Tasks", "Entries", "Total"],
  taskSubtask: ["Task", "Subtask", "Reference", "Entries", "Total"],
};

let activeTimer = null;
let taskItems = [];
let subtaskItems = [];
let pendingSelectedTaskId = null;
let tickHandle = null;
let createTaskMode = "free";
let createImportSearchHandle = null;
let createImportSearchRequest = 0;
let hasGithubToken = false;
let reportPeriod = "all";
let reportMode = "task";
let taskSummaryRows = [];
let subtaskSummaryRows = [];
let taskSubtaskSummaryRows = [];
let pendingCloseTaskId = null;
let pendingCloseResetHandle = null;

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
  renderTaskSuggestions();
}

function selectedTaskItem() {
  return taskItems.find((item) => String(item.task.id) === taskSelect.value) ?? null;
}

/// Subtasks already recorded against a task, most recently used first.
function subtasksForTask(taskId) {
  return taskItems.find((item) => item.task.id === taskId)?.subtasks ?? [];
}

function activeSubtaskNames() {
  return subtaskItems.filter((subtask) => !subtask.archivedAt).map((subtask) => subtask.name);
}

/// Names to offer for a task: the ones already used on it, then the rest of the
/// shared list. Archived subtasks are only offered if the task has used them.
function orderedSubtaskSuggestions(taskId) {
  const usedNames = subtasksForTask(taskId).map((subtask) => subtask.name);
  const seen = new Set();

  return [...usedNames, ...activeSubtaskNames()].filter((name) => {
    const key = name.toLowerCase();
    if (!name || seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function renderSubtaskOptions(datalist, taskId) {
  datalist.innerHTML = "";

  for (const name of orderedSubtaskSuggestions(taskId)) {
    const option = document.createElement("option");
    option.value = name;
    datalist.append(option);
  }
}

function renderSubtaskSuggestions() {
  renderSubtaskOptions(subtaskSuggestions, selectedTaskItem()?.task.id);
}

function updateSelectedTaskDetails() {
  const selected = selectedTaskItem();
  startTimerButton.disabled = !selected;
  closeTaskButton.disabled = !selected;
  renderSubtaskSuggestions();

  if (!selected || pendingCloseTaskId !== selected.task.id) {
    resetCloseConfirmation();
  }
}

function githubClosedTaskItems() {
  return taskItems.filter((item) => {
    const state = item.task.githubState?.toLowerCase();
    return state === "closed" && item.task.githubReference;
  });
}

function renderTaskSuggestions() {
  const suggestions = githubClosedTaskItems();
  taskSuggestions.innerHTML = "";
  taskSuggestions.hidden = suggestions.length === 0;

  for (const item of suggestions.slice(0, 3)) {
    const row = document.createElement("div");
    const text = document.createElement("span");
    const button = document.createElement("button");

    row.className = "task-suggestion";
    text.textContent = `${item.task.name} is closed on GitHub.`;
    button.type = "button";
    button.className = "secondary compact";
    button.textContent = "Close";
    button.addEventListener("click", () => closeTask(item.task));

    row.append(text, button);
    taskSuggestions.append(row);
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
    renderSubtaskOptions(datalist, entry.taskId);

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

function summaryRowsForMode() {
  if (reportMode === "subtask") return subtaskSummaryRows;
  if (reportMode === "taskSubtask") return taskSubtaskSummaryRows;
  return taskSummaryRows;
}

/// Cells for one summary row, matching REPORT_COLUMNS for the current mode.
function summaryCells(item) {
  const subtask = `<td class="muted">${escapeHtml(item.subtaskName ?? "No subtask")}</td>`;
  const entries = `<td class="mono">${item.entryCount}</td>`;
  const total = `<td class="mono">${formatDuration(item.totalSeconds)}</td>`;

  if (reportMode === "subtask") {
    return `
      <td>${escapeHtml(item.subtaskName ?? "No subtask")}</td>
      <td class="mono">${item.taskCount}</td>
      ${entries}
      ${total}
    `;
  }

  const task = `<td>${escapeHtml(item.taskName)}</td>`;
  const reference = `<td class="muted">${escapeHtml(referenceLabel(item))}</td>`;

  if (reportMode === "taskSubtask") {
    return `${task}${subtask}${reference}${entries}${total}`;
  }

  return `${task}${reference}${entries}${total}`;
}

function renderSummary() {
  const rows = summaryRowsForMode();
  const columns = REPORT_COLUMNS[reportMode];

  reportAllTimePeriod.classList.toggle("active", reportPeriod === "all");
  reportTodayPeriod.classList.toggle("active", reportPeriod === "today");
  reportThisWeekPeriod.classList.toggle("active", reportPeriod === "this_week");
  reportLastWeekPeriod.classList.toggle("active", reportPeriod === "last_week");
  reportThisMonthPeriod.classList.toggle("active", reportPeriod === "this_month");
  reportTaskMode.classList.toggle("active", reportMode === "task");
  reportSubtaskMode.classList.toggle("active", reportMode === "subtask");
  reportTaskSubtaskMode.classList.toggle("active", reportMode === "taskSubtask");
  reportTotalDuration.textContent = formatDuration(
    rows.reduce((total, item) => total + item.totalSeconds, 0),
  );

  summaryHeader.innerHTML = `<tr>${columns.map((name) => `<th>${name}</th>`).join("")}</tr>`;
  summaryRows.innerHTML = "";

  if (!rows.length) {
    summaryRows.innerHTML = `<tr><td colspan="${columns.length}" class="muted">No report data yet.</td></tr>`;
    return;
  }

  for (const item of rows) {
    const row = document.createElement("tr");
    row.innerHTML = summaryCells(item);
    summaryRows.append(row);
  }
}

function setReportMode(mode) {
  reportMode = mode;
  renderSummary();
}

/// Matches how the backend deduplicates subtask names.
function subtaskKey(name) {
  return name.trim().replace(/\s+/g, " ").toLowerCase();
}

/// The subtask a rename would merge into, if the new name is already taken.
function mergeTargetFor(subtask, name) {
  const key = subtaskKey(name);
  return subtaskItems.find((item) => item.id !== subtask.id && subtaskKey(item.name) === key);
}

/// One row of the subtask manager: rename in place, archive or restore.
function renderSubtaskRow(subtask) {
  const row = document.createElement("tr");
  const nameCell = document.createElement("td");
  const editor = document.createElement("div");
  const nameInput = document.createElement("input");
  const saveButton = document.createElement("button");
  const actionCell = document.createElement("td");
  const archiveButton = document.createElement("button");
  const isArchived = Boolean(subtask.archivedAt);
  let confirmMergeHandle = null;
  let awaitingMergeConfirmation = false;

  editor.className = "inline-edit";
  nameInput.type = "text";
  nameInput.value = subtask.name;
  nameInput.setAttribute("aria-label", `Name for ${subtask.name}`);
  saveButton.type = "button";
  saveButton.className = "secondary compact";
  saveButton.textContent = "Rename";
  saveButton.disabled = true;

  const resetMergeConfirmation = () => {
    window.clearTimeout(confirmMergeHandle);
    confirmMergeHandle = null;
    awaitingMergeConfirmation = false;
  };

  const syncSaveState = () => {
    const value = nameInput.value.trim();
    resetMergeConfirmation();
    saveButton.disabled = !value || value === subtask.name;
    saveButton.textContent = mergeTargetFor(subtask, value) ? "Merge" : "Rename";
  };

  // Merging pools two subtasks' entries and cannot be undone, so it takes a
  // second click to confirm.
  const submit = async () => {
    if (saveButton.disabled) return;

    const target = mergeTargetFor(subtask, nameInput.value);
    if (target && !awaitingMergeConfirmation) {
      awaitingMergeConfirmation = true;
      saveButton.textContent = "Confirm merge";
      subtaskStatus.textContent = `Merging moves every entry from "${subtask.name}" onto "${target.name}".`;
      confirmMergeHandle = window.setTimeout(() => {
        resetMergeConfirmation();
        saveButton.textContent = "Merge";
      }, 4000);
      return;
    }

    resetMergeConfirmation();
    saveButton.disabled = true;
    await runSubtaskAction(
      () => invoke("rename_subtask", { input: { subtaskId: subtask.id, name: nameInput.value } }),
      target
        ? `Merged "${subtask.name}" into "${target.name}".`
        : `Renamed "${subtask.name}" to "${nameInput.value.trim()}".`,
    );
  };

  nameInput.addEventListener("input", syncSaveState);
  nameInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      submit();
    }
  });
  saveButton.addEventListener("click", submit);
  editor.append(nameInput, saveButton);
  nameCell.append(editor);

  archiveButton.type = "button";
  archiveButton.className = "secondary compact";
  archiveButton.textContent = isArchived ? "Restore" : "Archive";
  archiveButton.addEventListener("click", () =>
    runSubtaskAction(
      () =>
        invoke("set_subtask_archived", {
          input: { subtaskId: subtask.id, archived: !isArchived },
        }),
      isArchived ? `Restored "${subtask.name}".` : `Archived "${subtask.name}".`,
    ),
  );
  actionCell.append(archiveButton);

  row.innerHTML = `
    <td class="mono">${subtask.entryCount}</td>
    <td class="mono">${formatDuration(subtask.totalSeconds)}</td>
  `;
  row.prepend(nameCell);
  row.append(actionCell);

  if (isArchived) {
    row.classList.add("archived");
    nameCell.append(archivedBadge());
  }

  return row;
}

function archivedBadge() {
  const badge = document.createElement("span");
  badge.className = "badge";
  badge.textContent = "Archived";
  return badge;
}

function renderSubtaskManager() {
  subtaskRows.innerHTML = "";

  if (!subtaskItems.length) {
    subtaskRows.innerHTML = `<tr><td colspan="4" class="muted">No subtasks yet. Start a timer with a subtask name, or add one above.</td></tr>`;
    return;
  }

  // Archived subtasks sink to the bottom; the rest keep the backend's ordering.
  const ordered = [
    ...subtaskItems.filter((subtask) => !subtask.archivedAt),
    ...subtaskItems.filter((subtask) => subtask.archivedAt),
  ];

  for (const subtask of ordered) {
    subtaskRows.append(renderSubtaskRow(subtask));
  }
}

/// Runs a subtask edit, reporting the outcome in the manager's status line.
/// Returns whether it succeeded.
async function runSubtaskAction(action, successMessage) {
  subtaskStatus.textContent = "";
  try {
    subtaskItems = await action();
    subtaskStatus.textContent = successMessage;
    renderSubtaskManager();
    renderSubtaskSuggestions();
    await refresh();
    return true;
  } catch (error) {
    subtaskStatus.textContent = String(error);
    renderSubtaskManager();
    return false;
  }
}

async function addSubtask() {
  const name = newSubtaskName.value.trim();
  if (!name) {
    subtaskStatus.textContent = "A subtask name is required.";
    return;
  }

  addSubtaskButton.disabled = true;
  const added = await runSubtaskAction(
    () => invoke("create_subtask", { input: { name } }),
    `Added "${name}".`,
  );
  addSubtaskButton.disabled = false;

  if (added) {
    newSubtaskName.value = "";
  }
  newSubtaskName.focus();
}

async function setReportPeriod(period) {
  if (reportPeriod === period) return;
  reportPeriod = period;
  renderSummary();
  await refresh();
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
    await refreshGithubTaskStates();
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

function resetCloseConfirmation() {
  window.clearTimeout(pendingCloseResetHandle);
  pendingCloseResetHandle = null;
  pendingCloseTaskId = null;
  closeTaskButton.textContent = "Close";
}

function requestCloseSelectedTask() {
  const task = selectedTaskItem()?.task;
  if (!task) return;

  if (pendingCloseTaskId === task.id) {
    closeTask(task);
    return;
  }

  pendingCloseTaskId = task.id;
  closeTaskButton.textContent = "Confirm";
  window.clearTimeout(pendingCloseResetHandle);
  pendingCloseResetHandle = window.setTimeout(resetCloseConfirmation, 4000);
}

async function closeTask(task = selectedTaskItem()?.task) {
  if (!task) return;

  closeTaskButton.disabled = true;
  try {
    await invoke("close_task", { input: { taskId: task.id } });
    resetCloseConfirmation();
    pendingSelectedTaskId = null;
    await refresh();
  } catch (error) {
    closeTaskButton.disabled = false;
    taskSuggestions.hidden = false;
    taskSuggestions.innerHTML = `<div class="task-suggestion"><span>${escapeHtml(String(error))}</span></div>`;
  }
}

async function refreshGithubTaskStates() {
  if (!hasGithubToken) return;

  try {
    const tasks = await invoke("refresh_github_task_states");
    renderTaskOptions(tasks);
  } catch (error) {
    console.error(error);
  }
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
      githubState: null,
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
          githubState: result.state,
        });
      } catch (error) {
        createTaskStatus.textContent = String(error);
      }
    });
    createImportResults.append(button);
  }
}

async function refresh() {
  const [tasks, subtasks, active, entries, taskSummary, subtaskSummary, taskSubtaskSummary] =
    await Promise.all([
      invoke("list_tasks"),
      invoke("list_subtasks"),
      invoke("get_active_timer"),
      invoke("recent_entries", { limit: 50 }),
      invoke("summary_by_task", { period: reportPeriod }),
      invoke("summary_by_subtask", { period: reportPeriod }),
      invoke("summary_by_task_and_subtask", { period: reportPeriod }),
    ]);

  activeTimer = active;
  subtaskItems = subtasks;
  taskSummaryRows = taskSummary;
  subtaskSummaryRows = subtaskSummary;
  taskSubtaskSummaryRows = taskSubtaskSummary;
  renderTaskOptions(tasks);
  renderActiveTimer();
  renderRecent(entries);
  renderSummary();
  renderSubtaskManager();
}

function asSentence(value) {
  return value ? value.charAt(0).toUpperCase() + value.slice(1) : value;
}

/// Reports a schema migration that could not be completed. Everything else in
/// the app will fail while this is the case, so say so once and clearly.
async function checkDatabaseStatus() {
  const status = await invoke("database_status");
  if (status.ok) return true;

  databaseErrorMessage.textContent = asSentence(
    status.message ?? "The database could not be opened.",
  );
  databaseErrorBackup.textContent = status.backupPath
    ? `Your data has not been changed. A copy of it from before the update is at ${status.backupPath}`
    : "Your data has not been changed.";
  databaseErrorDialog.hidden = false;
  return false;
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
    githubState: selected.task.githubState ?? null,
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
closeTaskButton.addEventListener("click", requestCloseSelectedTask);
reportAllTimePeriod.addEventListener("click", () => setReportPeriod("all"));
reportTodayPeriod.addEventListener("click", () => setReportPeriod("today"));
reportThisWeekPeriod.addEventListener("click", () => setReportPeriod("this_week"));
reportLastWeekPeriod.addEventListener("click", () => setReportPeriod("last_week"));
reportThisMonthPeriod.addEventListener("click", () => setReportPeriod("this_month"));
reportTaskMode.addEventListener("click", () => setReportMode("task"));
reportSubtaskMode.addEventListener("click", () => setReportMode("subtask"));
reportTaskSubtaskMode.addEventListener("click", () => setReportMode("taskSubtask"));
addSubtaskButton.addEventListener("click", addSubtask);
newSubtaskName.addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    addSubtask();
  }
});
databaseErrorClose.addEventListener("click", () => {
  databaseErrorDialog.hidden = true;
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

/// Refresh for event listeners, where a rejected promise has nowhere to go.
async function refreshSafely() {
  try {
    await refresh();
  } catch (error) {
    console.error(error);
  }
}

await listen("timer-updated", refreshSafely);
await listen("open-github-token-settings", openGithubTokenDialog);

// Surface a failed schema update before loading, then load anyway so whatever
// still works is available.
await checkDatabaseStatus();

try {
  await loadGithubToken();
  await refresh();
  await refreshGithubTaskStates();
} catch (error) {
  console.error(error);
}

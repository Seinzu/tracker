use chrono::{
    DateTime, Datelike, Days, Duration as ChronoDuration, Local, LocalResult, NaiveDate,
    SecondsFormat, TimeZone, Utc,
};
use keyring::v1::{Entry, Error as KeyringError};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tauri::menu::{Menu, MenuBuilder, SubmenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use thiserror::Error;

mod db;

const TRAY_ID: &str = "tracker-tray";
const TRAY_TASK_PREFIX: &str = "start-task:";
const MENU_GITHUB_TOKEN_ID: &str = "github-token-settings";
const MENU_SHOW_ID: &str = "show-tracker";
const GITHUB_KEYCHAIN_SERVICE: &str = "dev.local.tracker";
const GITHUB_KEYCHAIN_USER: &str = "github-token";

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
    backup_path: Option<PathBuf>,
    /// Set when the schema could not be brought up to date at startup. Every
    /// database access fails with this message rather than reading a schema the
    /// queries below do not match.
    migration_error: Option<String>,
}

impl AppState {
    fn connect(&self) -> Result<Connection, TrackerError> {
        if let Some(message) = self.migration_error.as_deref() {
            return Err(TrackerError::DatabaseUnavailable(message.to_owned()));
        }

        db::open(&self.db_path)
    }
}

#[derive(Debug, Error)]
enum TrackerError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("task name is required")]
    MissingTaskName,
    #[error("subtask name is required")]
    MissingSubtaskName,
    #[error("application data directory is not available")]
    MissingDataDir,
    #[error("tauri error: {0}")]
    Tauri(#[from] tauri::Error),
    #[error("github request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("keychain error: {0}")]
    Keychain(#[from] KeyringError),
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "this database uses schema version {found}, but this version of Tracker only supports version {supported}. Update Tracker to open it."
    )]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("no migration is defined for schema version {0}")]
    MissingMigration(i64),
    #[error("the migration would have left {0} time entries pointing at missing rows")]
    MigrationIntegrity(i64),
    #[error("the database could not be opened: {0}")]
    DatabaseUnavailable(String),
}

impl From<TrackerError> for String {
    fn from(error: TrackerError) -> Self {
        error.to_string()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Task {
    id: i64,
    name: String,
    github_kind: Option<String>,
    github_reference: Option<String>,
    github_state: Option<String>,
    github_checked_at: Option<String>,
    closed_at: Option<String>,
    created_at: String,
    updated_at: String,
}

/// A subtask, shared across every task.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Subtask {
    id: i64,
    name: String,
    archived_at: Option<String>,
    created_at: String,
}

/// A subtask together with how much it has been used, for the management view.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubtaskView {
    id: i64,
    name: String,
    archived_at: Option<String>,
    created_at: String,
    entry_count: i64,
    total_seconds: i64,
}

/// A task alongside the subtasks already recorded against it, most recently
/// used first. Subtasks are shared, so this is a usage history rather than a
/// set of subtasks the task owns.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskWithSubtasks {
    task: Task,
    subtasks: Vec<Subtask>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskInput {
    name: String,
    github_kind: Option<String>,
    github_reference: Option<String>,
    github_state: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartTimerInput {
    task: TaskInput,
    subtask_name: Option<String>,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateEntrySubtaskInput {
    entry_id: i64,
    subtask_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloseTaskInput {
    task_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSubtaskInput {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameSubtaskInput {
    subtask_id: i64,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetSubtaskArchivedInput {
    subtask_id: i64,
    archived: bool,
}

/// Reported to the UI at startup so a failed migration can be explained
/// instead of surfacing as an error on every action.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DatabaseStatus {
    ok: bool,
    /// Schema version this build reads and writes.
    schema_version: i64,
    /// Version found in the file, absent when it could not be read.
    database_version: Option<i64>,
    message: Option<String>,
    backup_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActiveTimer {
    entry_id: i64,
    task: Task,
    subtask: Option<Subtask>,
    started_at: String,
    elapsed_seconds: i64,
    note: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TimeEntryView {
    id: i64,
    task_id: i64,
    task_name: String,
    subtask_id: Option<i64>,
    subtask_name: Option<String>,
    github_kind: Option<String>,
    github_reference: Option<String>,
    started_at: String,
    ended_at: Option<String>,
    duration_seconds: i64,
    note: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SummaryRow {
    task_id: i64,
    task_name: String,
    subtask_id: Option<i64>,
    subtask_name: Option<String>,
    github_kind: Option<String>,
    github_reference: Option<String>,
    total_seconds: i64,
    entry_count: i64,
}

/// Time spent on a subtask across every task, so questions like "how long did
/// I spend reviewing code this month" have a single row to read.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubtaskSummaryRow {
    subtask_id: Option<i64>,
    subtask_name: Option<String>,
    task_count: i64,
    total_seconds: i64,
    entry_count: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GithubSearchInput {
    query: String,
    github_kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GithubTokenInput {
    token: Option<String>,
}

struct ReportRange {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GithubSearchResult {
    title: String,
    reference: String,
    url: String,
    repository: String,
    number: i64,
    state: String,
}

#[derive(Debug, Deserialize)]
struct GithubSearchResponse {
    items: Vec<GithubIssue>,
}

#[derive(Debug, Deserialize)]
struct GithubIssue {
    html_url: String,
    repository_url: String,
    number: i64,
    title: String,
    state: String,
    pull_request: Option<serde_json::Value>,
}

#[tauri::command]
fn database_status(state: State<'_, AppState>) -> DatabaseStatus {
    database_status_inner(&state)
}

#[tauri::command]
fn list_tasks(state: State<'_, AppState>) -> Result<Vec<TaskWithSubtasks>, String> {
    let conn = state.connect().map_err(String::from)?;
    list_tasks_inner(&conn).map_err(String::from)
}

#[tauri::command]
fn list_subtasks(state: State<'_, AppState>) -> Result<Vec<SubtaskView>, String> {
    let conn = state.connect().map_err(String::from)?;
    list_subtasks_inner(&conn).map_err(String::from)
}

#[tauri::command]
fn create_subtask(
    state: State<'_, AppState>,
    input: CreateSubtaskInput,
) -> Result<Vec<SubtaskView>, String> {
    create_subtask_inner(&state, input).map_err(String::from)
}

#[tauri::command]
fn rename_subtask(
    state: State<'_, AppState>,
    input: RenameSubtaskInput,
    app: AppHandle,
) -> Result<Vec<SubtaskView>, String> {
    let subtasks = rename_subtask_inner(&state, input).map_err(String::from)?;
    // A rename changes the label the tray and the recent entries show.
    let _ = refresh_tray_status(&app);
    let _ = app.emit("timer-updated", ());
    Ok(subtasks)
}

#[tauri::command]
fn set_subtask_archived(
    state: State<'_, AppState>,
    input: SetSubtaskArchivedInput,
) -> Result<Vec<SubtaskView>, String> {
    set_subtask_archived_inner(&state, input).map_err(String::from)
}

#[tauri::command]
fn create_task(
    state: State<'_, AppState>,
    input: TaskInput,
    app: AppHandle,
) -> Result<TaskWithSubtasks, String> {
    let created = create_task_inner(&state, input).map_err(String::from)?;
    let _ = refresh_tray_menu(&app);
    let _ = app.emit("tasks-updated", ());
    Ok(created)
}

#[tauri::command]
fn close_task(
    state: State<'_, AppState>,
    input: CloseTaskInput,
    app: AppHandle,
) -> Result<(), String> {
    close_task_inner(&state, input.task_id).map_err(String::from)?;
    let _ = refresh_tray_menu(&app);
    let _ = refresh_tray_status(&app);
    let _ = app.emit("timer-updated", ());
    Ok(())
}

#[tauri::command]
fn start_timer(
    state: State<'_, AppState>,
    input: StartTimerInput,
    app: AppHandle,
) -> Result<ActiveTimer, String> {
    let timer = start_timer_inner(&state, input).map_err(String::from)?;
    let _ = refresh_tray_menu(&app);
    let _ = refresh_tray_status(&app);
    let _ = app.emit("timer-updated", ());
    Ok(timer)
}

#[tauri::command]
fn stop_timer(state: State<'_, AppState>, app: AppHandle) -> Result<Option<TimeEntryView>, String> {
    let stopped = stop_timer_inner(&state).map_err(String::from)?;
    let _ = refresh_tray_status(&app);
    let _ = app.emit("timer-updated", ());
    Ok(stopped)
}

#[tauri::command]
fn get_active_timer(state: State<'_, AppState>) -> Result<Option<ActiveTimer>, String> {
    let conn = state.connect().map_err(String::from)?;
    active_timer_inner(&conn).map_err(String::from)
}

#[tauri::command]
fn recent_entries(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<TimeEntryView>, String> {
    let conn = state.connect().map_err(String::from)?;
    recent_entries_inner(&conn, limit.unwrap_or(20).clamp(1, 200)).map_err(String::from)
}

#[tauri::command]
fn update_time_entry_subtask(
    state: State<'_, AppState>,
    input: UpdateEntrySubtaskInput,
    app: AppHandle,
) -> Result<TimeEntryView, String> {
    let entry = update_time_entry_subtask_inner(&state, input).map_err(String::from)?;
    let _ = app.emit("timer-updated", ());
    Ok(entry)
}

#[tauri::command]
fn summary_by_task(
    state: State<'_, AppState>,
    period: Option<String>,
) -> Result<Vec<SummaryRow>, String> {
    let conn = state.connect().map_err(String::from)?;
    let range = report_range_for_period(period).map_err(String::from)?;
    summary_by_task_inner(&conn, range.as_ref()).map_err(String::from)
}

#[tauri::command]
fn summary_by_task_and_subtask(
    state: State<'_, AppState>,
    period: Option<String>,
) -> Result<Vec<SummaryRow>, String> {
    let conn = state.connect().map_err(String::from)?;
    let range = report_range_for_period(period).map_err(String::from)?;
    summary_by_task_and_subtask_inner(&conn, range.as_ref()).map_err(String::from)
}

#[tauri::command]
fn summary_by_subtask(
    state: State<'_, AppState>,
    period: Option<String>,
) -> Result<Vec<SubtaskSummaryRow>, String> {
    let conn = state.connect().map_err(String::from)?;
    let range = report_range_for_period(period).map_err(String::from)?;
    summary_by_subtask_inner(&conn, range.as_ref()).map_err(String::from)
}

#[tauri::command]
fn search_github_references(input: GithubSearchInput) -> Result<Vec<GithubSearchResult>, String> {
    search_github_references_inner(input).map_err(String::from)
}

#[tauri::command]
fn get_github_token() -> Result<Option<String>, String> {
    get_github_token_inner().map_err(String::from)
}

#[tauri::command]
fn set_github_token(input: GithubTokenInput) -> Result<(), String> {
    set_github_token_inner(input).map_err(String::from)
}

#[tauri::command]
fn refresh_github_task_states(state: State<'_, AppState>) -> Result<Vec<TaskWithSubtasks>, String> {
    refresh_github_task_states_inner(&state).map_err(String::from)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|_| TrackerError::MissingDataDir)?;
            std::fs::create_dir_all(&data_dir)?;

            // Bring the schema up to date before anything reads from it. A
            // failure is recorded on the state rather than aborting startup,
            // so the app can open and explain what happened.
            let db_path = data_dir.join("tracker.sqlite3");
            let prepared = db::prepare(&db_path);
            let migration_error = prepared.result.err().map(|error| error.to_string());

            app.manage(AppState {
                db_path,
                backup_path: prepared.backup_path,
                migration_error,
            });
            build_app_menu(app.handle())?;
            build_tray(app.handle())?;
            start_tray_status_updater(app.handle());
            Ok(())
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_GITHUB_TOKEN_ID => {
                show_main_window(app);
                let _ = app.emit("open-github-token-settings", ());
            }
            MENU_SHOW_ID => show_main_window(app),
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            database_status,
            list_tasks,
            create_task,
            close_task,
            list_subtasks,
            create_subtask,
            rename_subtask,
            set_subtask_archived,
            start_timer,
            stop_timer,
            get_active_timer,
            recent_entries,
            update_time_entry_subtask,
            summary_by_task,
            summary_by_task_and_subtask,
            summary_by_subtask,
            search_github_references,
            get_github_token,
            set_github_token,
            refresh_github_task_states
        ])
        .run(tauri::generate_context!())
        .expect("failed to run tracker");
}

fn build_app_menu(app: &AppHandle) -> Result<(), TrackerError> {
    let app_menu = SubmenuBuilder::new(app, "Tracker")
        .about(None)
        .separator()
        .text(MENU_GITHUB_TOKEN_ID, "GitHub Token...")
        .text(MENU_SHOW_ID, "Show Tracker")
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let view_menu = SubmenuBuilder::new(app, "View").fullscreen().build()?;
    let window_menu = SubmenuBuilder::new(app, "Window")
        .minimize()
        .close_window()
        .build()?;

    let menu = Menu::with_items(app, &[&app_menu, &edit_menu, &view_menu, &window_menu])?;
    app.set_menu(menu)?;
    Ok(())
}

fn build_tray(app: &AppHandle) -> Result<(), TrackerError> {
    let menu = build_tray_menu(app)?;
    let status = tray_status_label(app)?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .title(&status)
        .tooltip(&status);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            "stop" => {
                let state = app.state::<AppState>();
                let _ = stop_timer_inner(&state);
                let _ = refresh_tray_status(app);
                let _ = app.emit("timer-updated", ());
            }
            "quit" => app.exit(0),
            id if id.starts_with(TRAY_TASK_PREFIX) => {
                if let Some(task_id) = id
                    .strip_prefix(TRAY_TASK_PREFIX)
                    .and_then(|value| value.parse::<i64>().ok())
                {
                    let state = app.state::<AppState>();
                    let _ = start_existing_task_inner(&state, task_id);
                    let _ = refresh_tray_menu(app);
                    let _ = refresh_tray_status(app);
                    let _ = app.emit("timer-updated", ());
                }
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

fn build_tray_menu(app: &AppHandle) -> Result<Menu<tauri::Wry>, TrackerError> {
    // An unreadable database still gets a usable tray menu, just without the
    // task shortcuts.
    let tasks = tray_tasks(app).unwrap_or_default();

    let mut builder = MenuBuilder::new(app)
        .text("show", "Show Tracker")
        .text("stop", "Stop Timing");

    if !tasks.is_empty() {
        builder = builder.separator();
        for item in tasks.into_iter().take(8) {
            builder = builder.text(
                format!("{TRAY_TASK_PREFIX}{}", item.task.id),
                format!("Start {}", tray_label(&item.task.name)),
            );
        }
    }

    builder
        .separator()
        .text("quit", "Quit")
        .build()
        .map_err(TrackerError::from)
}

fn tray_tasks(app: &AppHandle) -> Result<Vec<TaskWithSubtasks>, TrackerError> {
    let state = app.state::<AppState>();
    let conn = state.connect()?;
    list_tasks_inner(&conn)
}

fn refresh_tray_menu(app: &AppHandle) -> Result<(), TrackerError> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(build_tray_menu(app)?))?;
    }
    Ok(())
}

fn refresh_tray_status(app: &AppHandle) -> Result<(), TrackerError> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let status = tray_status_label(app)?;
        tray.set_title(Some(status.as_str()))?;
        tray.set_tooltip(Some(status.as_str()))?;
    }
    Ok(())
}

fn start_tray_status_updater(app: &AppHandle) {
    let app = app.clone();
    thread::spawn(move || {
        loop {
            let _ = refresh_tray_status(&app);
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn tray_status_label(app: &AppHandle) -> Result<String, TrackerError> {
    let state = app.state::<AppState>();
    let Ok(conn) = state.connect() else {
        return Ok("database error".to_owned());
    };
    let Some(active) = active_timer_inner(&conn)? else {
        return Ok("stopped".to_owned());
    };

    Ok(active_timer_tray_label(&active))
}

fn active_timer_tray_label(active: &ActiveTimer) -> String {
    let task_label = match active.subtask.as_ref() {
        Some(subtask) => format!("{} / {}", active.task.name, subtask.name),
        None => active.task.name.clone(),
    };
    let elapsed = format_duration_hms(active.elapsed_seconds);
    format!("{} {}", truncate_chars(&task_label, 34), elapsed)
}

fn format_duration_hms(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn tray_label(value: &str) -> String {
    const MAX_CHARS: usize = 42;
    truncate_chars(value, MAX_CHARS)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }

    let mut label = value.chars().take(max_chars - 1).collect::<String>();
    label.push_str("...");
    label
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn search_github_references_inner(
    input: GithubSearchInput,
) -> Result<Vec<GithubSearchResult>, TrackerError> {
    let query = input.query.trim();
    if query.len() < 3 {
        return Ok(Vec::new());
    }

    let client = github_client()?;
    let kind = normalize_optional(input.github_kind).unwrap_or_else(|| "issue".to_owned());
    let token = get_github_token_inner()?;

    if let Some((owner, repo, number)) = parse_github_reference(query) {
        let url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}");
        let issue: GithubIssue = github_auth(client.get(url), token.as_deref())
            .send()?
            .json()?;
        return Ok(issue_to_result(issue).into_iter().collect());
    }

    let search_query = github_search_query(query, &kind);
    let response: GithubSearchResponse = github_auth(
        client
            .get("https://api.github.com/search/issues")
            .query(&[("q", search_query.as_str()), ("per_page", "8")]),
        token.as_deref(),
    )
    .send()?
    .json()?;

    let wants_pr = kind == "pull_request";
    Ok(response
        .items
        .into_iter()
        .filter(|item| item.pull_request.is_some() == wants_pr)
        .filter_map(issue_to_result)
        .collect())
}

fn get_github_token_inner() -> Result<Option<String>, TrackerError> {
    match github_token_entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(TrackerError::Keychain(error)),
    }
}

fn set_github_token_inner(input: GithubTokenInput) -> Result<(), TrackerError> {
    let entry = github_token_entry()?;

    match normalize_optional(input.token) {
        Some(token) => entry.set_password(&token)?,
        None => match entry.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => {}
            Err(error) => return Err(TrackerError::Keychain(error)),
        },
    }

    Ok(())
}

fn github_token_entry() -> Result<Entry, TrackerError> {
    Entry::new(GITHUB_KEYCHAIN_SERVICE, GITHUB_KEYCHAIN_USER).map_err(TrackerError::from)
}

fn github_client() -> Result<Client, TrackerError> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("tracker-tauri-app"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );

    Ok(Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(8))
        .build()?)
}

fn github_auth(request: RequestBuilder, token: Option<&str>) -> RequestBuilder {
    match token {
        Some(token) => request.header(AUTHORIZATION, format!("Bearer {token}")),
        None => request,
    }
}

fn github_search_query(query: &str, kind: &str) -> String {
    let github_type = if kind == "pull_request" {
        "pr"
    } else {
        "issue"
    };

    if let Some((repo, search)) = split_repo_query(query) {
        return format!("{search} repo:{repo} type:{github_type}");
    }

    format!("{query} type:{github_type}")
}

fn split_repo_query(query: &str) -> Option<(String, String)> {
    let mut parts = query.splitn(2, char::is_whitespace);
    let repo = parts.next()?.trim();
    let search = parts.next()?.trim();

    if is_repo_name(repo) && !search.is_empty() {
        Some((repo.to_owned(), search.to_owned()))
    } else {
        None
    }
}

fn parse_github_reference(value: &str) -> Option<(String, String, i64)> {
    if let Some(rest) = value.strip_prefix("https://github.com/") {
        let mut parts = rest.split('/');
        let owner = parts.next()?.to_owned();
        let repo = parts.next()?.to_owned();
        let marker = parts.next()?;
        let number = parts.next()?.parse().ok()?;

        if marker == "issues" || marker == "pull" {
            return Some((owner, repo, number));
        }
    }

    let (repo, number) = value.split_once('#')?;
    let mut repo_parts = repo.split('/');
    let owner = repo_parts.next()?.trim();
    let name = repo_parts.next()?.trim();

    if repo_parts.next().is_some() || !is_repo_name(repo) {
        return None;
    }

    Some((
        owner.to_owned(),
        name.to_owned(),
        number.trim().parse().ok()?,
    ))
}

fn is_repo_name(value: &str) -> bool {
    let mut parts = value.split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(owner), Some(repo), None) if !owner.is_empty() && !repo.is_empty())
}

fn issue_to_result(issue: GithubIssue) -> Option<GithubSearchResult> {
    let repository = issue
        .repository_url
        .strip_prefix("https://api.github.com/repos/")?
        .to_owned();

    Some(GithubSearchResult {
        reference: format!("{repository}#{}", issue.number),
        title: issue.title,
        url: issue.html_url,
        repository,
        number: issue.number,
        state: issue.state,
    })
}

fn github_issue_state_for_reference(
    client: &Client,
    token: Option<&str>,
    reference: &str,
) -> Result<Option<String>, TrackerError> {
    let Some((owner, repo, number)) = parse_github_reference(reference) else {
        return Ok(None);
    };

    let url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}");
    let issue: GithubIssue = github_auth(client.get(url), token).send()?.json()?;
    Ok(Some(issue.state))
}

fn should_refresh_github_task_state(task: &Task) -> bool {
    if !matches!(
        task.github_kind.as_deref(),
        Some("issue") | Some("pull_request")
    ) || task.github_reference.is_none()
    {
        return false;
    }

    let Some(checked_at) = task.github_checked_at.as_deref() else {
        return true;
    };
    let Ok(checked_at) = DateTime::parse_from_rfc3339(checked_at) else {
        return true;
    };

    checked_at.with_timezone(&Utc) < Utc::now() - ChronoDuration::minutes(30)
}

fn database_status_inner(state: &State<'_, AppState>) -> DatabaseStatus {
    let backup_path = state
        .backup_path
        .as_ref()
        .map(|path| path.display().to_string());

    // Read the version straight from the file, which works even when the
    // migration that should have updated it failed.
    let database_version = db::open(&state.db_path)
        .and_then(|conn| db::schema_version(&conn))
        .ok();

    DatabaseStatus {
        ok: state.migration_error.is_none(),
        schema_version: db::SCHEMA_VERSION,
        database_version,
        message: state.migration_error.clone(),
        backup_path,
    }
}

fn create_task_inner(
    state: &State<'_, AppState>,
    input: TaskInput,
) -> Result<TaskWithSubtasks, TrackerError> {
    let mut conn = state.connect()?;
    let tx = conn.transaction()?;
    let now = now_string();
    let task = upsert_task(&tx, &input, &now)?;
    tx.commit()?;

    let conn = state.connect()?;
    Ok(TaskWithSubtasks {
        subtasks: subtasks_for_task(&conn, task.id)?,
        task,
    })
}

fn close_task_inner(state: &State<'_, AppState>, task_id: i64) -> Result<(), TrackerError> {
    let mut conn = state.connect()?;
    close_task_conn(&mut conn, task_id)
}

fn close_task_conn(conn: &mut Connection, task_id: i64) -> Result<(), TrackerError> {
    let tx = conn.transaction()?;
    let now = now_string();

    tx.execute(
        "
        UPDATE tasks
        SET closed_at = ?1, updated_at = ?1
        WHERE id = ?2
        ",
        params![now, task_id],
    )?;
    tx.execute(
        "
        UPDATE time_entries
        SET ended_at = ?1
        WHERE task_id = ?2 AND ended_at IS NULL
        ",
        params![now, task_id],
    )?;
    tx.commit()?;

    Ok(())
}

fn refresh_github_task_states_inner(
    state: &State<'_, AppState>,
) -> Result<Vec<TaskWithSubtasks>, TrackerError> {
    let token = get_github_token_inner()?;
    let Some(token) = token.filter(|token| !token.trim().is_empty()) else {
        let conn = state.connect()?;
        return list_tasks_inner(&conn);
    };

    let conn = state.connect()?;
    let tasks = list_tasks_inner(&conn)?;
    drop(conn);

    let client = github_client()?;
    for item in tasks {
        let task = item.task;
        if !should_refresh_github_task_state(&task) {
            continue;
        }

        let Some(reference) = task.github_reference.as_deref() else {
            continue;
        };
        let Some(github_state) =
            github_issue_state_for_reference(&client, Some(token.as_str()), reference)?
        else {
            continue;
        };

        let conn = state.connect()?;
        conn.execute(
            "
            UPDATE tasks
            SET github_state = ?1, github_checked_at = ?2
            WHERE id = ?3
            ",
            params![github_state, now_string(), task.id],
        )?;
    }

    let conn = state.connect()?;
    list_tasks_inner(&conn)
}

fn start_timer_inner(
    state: &State<'_, AppState>,
    input: StartTimerInput,
) -> Result<ActiveTimer, TrackerError> {
    let mut conn = state.connect()?;
    let tx = conn.transaction()?;
    let now = now_string();

    stop_active_entries(&tx, &now)?;

    let task = upsert_task(&tx, &input.task, &now)?;
    let subtask = match normalize_optional(input.subtask_name) {
        Some(name) => Some(upsert_subtask(&tx, &name, &now)?),
        None => None,
    };

    tx.execute(
        "INSERT INTO time_entries (task_id, subtask_id, started_at, note)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            task.id,
            subtask.as_ref().map(|item| item.id),
            now,
            normalize_optional(input.note)
        ],
    )?;

    let entry_id = tx.last_insert_rowid();
    tx.commit()?;

    Ok(ActiveTimer {
        entry_id,
        task,
        subtask,
        started_at: now,
        elapsed_seconds: 0,
        note: None,
    })
}

fn stop_timer_inner(state: &State<'_, AppState>) -> Result<Option<TimeEntryView>, TrackerError> {
    let mut conn = state.connect()?;
    let tx = conn.transaction()?;
    let active_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM time_entries WHERE ended_at IS NULL ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    let Some(active_id) = active_id else {
        return Ok(None);
    };

    let now = now_string();
    tx.execute(
        "UPDATE time_entries SET ended_at = ?1 WHERE id = ?2",
        params![now, active_id],
    )?;
    tx.commit()?;

    let conn = state.connect()?;
    entry_view_by_id(&conn, active_id).map(Some)
}

fn start_existing_task_inner(
    state: &State<'_, AppState>,
    task_id: i64,
) -> Result<ActiveTimer, TrackerError> {
    let conn = state.connect()?;
    let task = task_by_id_conn(&conn, task_id)?;
    let subtask_name = latest_subtask_name_for_task(&conn, task_id)?;
    drop(conn);

    start_timer_inner(
        state,
        StartTimerInput {
            task: TaskInput {
                name: task.name,
                github_kind: task.github_kind,
                github_reference: task.github_reference,
                github_state: task.github_state,
            },
            subtask_name,
            note: None,
        },
    )
}

fn active_timer_inner(conn: &Connection) -> Result<Option<ActiveTimer>, TrackerError> {
    let row = conn
        .query_row(
            "
            SELECT e.id, e.started_at, e.note,
                   t.id, t.name, t.github_kind, t.github_reference,
                   t.github_state, t.github_checked_at, t.closed_at, t.created_at, t.updated_at,
                   s.id, s.name, s.archived_at, s.created_at
            FROM time_entries e
            JOIN tasks t ON t.id = e.task_id
            LEFT JOIN subtasks s ON s.id = e.subtask_id
            WHERE e.ended_at IS NULL
            ORDER BY e.started_at DESC
            LIMIT 1
            ",
            [],
            |row| {
                let started_at: String = row.get(1)?;
                let subtask_id: Option<i64> = row.get(12)?;
                let subtask = match subtask_id {
                    Some(id) => Some(Subtask {
                        id,
                        name: row.get(13)?,
                        archived_at: row.get(14)?,
                        created_at: row.get(15)?,
                    }),
                    None => None,
                };

                Ok(ActiveTimer {
                    entry_id: row.get(0)?,
                    task: Task {
                        id: row.get(3)?,
                        name: row.get(4)?,
                        github_kind: row.get(5)?,
                        github_reference: row.get(6)?,
                        github_state: row.get(7)?,
                        github_checked_at: row.get(8)?,
                        closed_at: row.get(9)?,
                        created_at: row.get(10)?,
                        updated_at: row.get(11)?,
                    },
                    subtask,
                    elapsed_seconds: elapsed_seconds(&started_at, None).unwrap_or_default(),
                    started_at,
                    note: row.get(2)?,
                })
            },
        )
        .optional()?;

    Ok(row)
}

fn recent_entries_inner(conn: &Connection, limit: i64) -> Result<Vec<TimeEntryView>, TrackerError> {
    let mut stmt = conn.prepare(
        "
        SELECT e.id, e.task_id, t.name, e.subtask_id, s.name,
               t.github_kind, t.github_reference, e.started_at, e.ended_at, e.note
        FROM time_entries e
        JOIN tasks t ON t.id = e.task_id
        LEFT JOIN subtasks s ON s.id = e.subtask_id
        ORDER BY e.started_at DESC
        LIMIT ?1
        ",
    )?;

    let rows = stmt.query_map(params![limit], row_to_entry_view)?;
    collect_rows(rows)
}

fn report_entries_inner(
    conn: &Connection,
    range: Option<&ReportRange>,
) -> Result<Vec<TimeEntryView>, TrackerError> {
    let mut entries = match range {
        Some(range) => {
            let start = format_utc(range.start);
            let end = format_utc(range.end);
            let now = now_string();
            let mut stmt = conn.prepare(
                "
                SELECT e.id, e.task_id, t.name, e.subtask_id, s.name,
                       t.github_kind, t.github_reference, e.started_at, e.ended_at, e.note
                FROM time_entries e
                JOIN tasks t ON t.id = e.task_id
                LEFT JOIN subtasks s ON s.id = e.subtask_id
                WHERE e.started_at < ?2
                  AND COALESCE(e.ended_at, ?3) >= ?1
                ORDER BY e.started_at DESC
                ",
            )?;
            let rows = stmt.query_map(params![start, end, now], row_to_entry_view)?;
            collect_rows(rows)?
        }
        None => recent_entries_inner(conn, 10_000)?,
    };

    if let Some(range) = range {
        for entry in &mut entries {
            entry.duration_seconds =
                elapsed_seconds_in_range(&entry.started_at, entry.ended_at.as_deref(), range)?;
        }
        entries.retain(|entry| entry.duration_seconds > 0);
    }

    Ok(entries)
}

fn update_time_entry_subtask_inner(
    state: &State<'_, AppState>,
    input: UpdateEntrySubtaskInput,
) -> Result<TimeEntryView, TrackerError> {
    let mut conn = state.connect()?;
    let entry_id = input.entry_id;

    {
        let tx = conn.transaction()?;
        // Fails when the entry has gone, before any subtask is created for it.
        let _: i64 = tx.query_row(
            "SELECT id FROM time_entries WHERE id = ?1",
            params![entry_id],
            |row| row.get(0),
        )?;
        let now = now_string();
        let subtask_id = match normalize_optional(input.subtask_name) {
            Some(name) => Some(upsert_subtask(&tx, &name, &now)?.id),
            None => None,
        };

        tx.execute(
            "UPDATE time_entries SET subtask_id = ?1 WHERE id = ?2",
            params![subtask_id, entry_id],
        )?;
        tx.commit()?;
    }

    entry_view_by_id(&conn, entry_id)
}

fn summary_by_task_inner(
    conn: &Connection,
    range: Option<&ReportRange>,
) -> Result<Vec<SummaryRow>, TrackerError> {
    let entries = report_entries_inner(conn, range)?;
    let mut rows: Vec<SummaryRow> = Vec::new();

    for entry in entries {
        if let Some(row) = rows.iter_mut().find(|row| row.task_id == entry.task_id) {
            row.total_seconds += entry.duration_seconds;
            row.entry_count += 1;
            continue;
        }

        rows.push(SummaryRow {
            task_id: entry.task_id,
            task_name: entry.task_name,
            subtask_id: None,
            subtask_name: None,
            github_kind: entry.github_kind,
            github_reference: entry.github_reference,
            total_seconds: entry.duration_seconds,
            entry_count: 1,
        });
    }

    rows.sort_by(|a, b| b.total_seconds.cmp(&a.total_seconds));
    Ok(rows)
}

fn summary_by_task_and_subtask_inner(
    conn: &Connection,
    range: Option<&ReportRange>,
) -> Result<Vec<SummaryRow>, TrackerError> {
    let entries = report_entries_inner(conn, range)?;
    let mut rows: Vec<SummaryRow> = Vec::new();

    for entry in entries {
        if let Some(row) = rows
            .iter_mut()
            .find(|row| row.task_id == entry.task_id && row.subtask_id == entry.subtask_id)
        {
            row.total_seconds += entry.duration_seconds;
            row.entry_count += 1;
            continue;
        }

        rows.push(SummaryRow {
            task_id: entry.task_id,
            task_name: entry.task_name,
            subtask_id: entry.subtask_id,
            subtask_name: entry.subtask_name,
            github_kind: entry.github_kind,
            github_reference: entry.github_reference,
            total_seconds: entry.duration_seconds,
            entry_count: 1,
        });
    }

    rows.sort_by(|a, b| {
        a.task_name
            .cmp(&b.task_name)
            .then_with(|| b.total_seconds.cmp(&a.total_seconds))
    });
    Ok(rows)
}

/// Totals time per subtask across every task. Entries without a subtask are
/// collected into a single unnamed row.
fn summary_by_subtask_inner(
    conn: &Connection,
    range: Option<&ReportRange>,
) -> Result<Vec<SubtaskSummaryRow>, TrackerError> {
    let entries = report_entries_inner(conn, range)?;
    let mut rows: Vec<SubtaskSummaryRow> = Vec::new();
    let mut task_ids: Vec<Vec<i64>> = Vec::new();

    for entry in entries {
        match rows
            .iter()
            .position(|row| row.subtask_id == entry.subtask_id)
        {
            Some(index) => {
                rows[index].total_seconds += entry.duration_seconds;
                rows[index].entry_count += 1;
                if !task_ids[index].contains(&entry.task_id) {
                    task_ids[index].push(entry.task_id);
                    rows[index].task_count += 1;
                }
            }
            None => {
                rows.push(SubtaskSummaryRow {
                    subtask_id: entry.subtask_id,
                    subtask_name: entry.subtask_name,
                    task_count: 1,
                    total_seconds: entry.duration_seconds,
                    entry_count: 1,
                });
                task_ids.push(vec![entry.task_id]);
            }
        }
    }

    rows.sort_by(|a, b| b.total_seconds.cmp(&a.total_seconds));
    Ok(rows)
}

fn entry_view_by_id(conn: &Connection, id: i64) -> Result<TimeEntryView, TrackerError> {
    conn.query_row(
        "
        SELECT e.id, e.task_id, t.name, e.subtask_id, s.name,
               t.github_kind, t.github_reference, e.started_at, e.ended_at, e.note
        FROM time_entries e
        JOIN tasks t ON t.id = e.task_id
        LEFT JOIN subtasks s ON s.id = e.subtask_id
        WHERE e.id = ?1
        ",
        params![id],
        row_to_entry_view,
    )
    .map_err(TrackerError::from)
}

fn list_tasks_inner(conn: &Connection) -> Result<Vec<TaskWithSubtasks>, TrackerError> {
    let mut stmt = conn.prepare(
        "
        SELECT id, name, github_kind, github_reference, github_state,
               github_checked_at, closed_at, created_at, updated_at
        FROM tasks
        WHERE closed_at IS NULL
        ORDER BY updated_at DESC, name ASC
        ",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Task {
            id: row.get(0)?,
            name: row.get(1)?,
            github_kind: row.get(2)?,
            github_reference: row.get(3)?,
            github_state: row.get(4)?,
            github_checked_at: row.get(5)?,
            closed_at: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;

    let mut tasks = Vec::new();
    for task in rows {
        let task = task?;
        tasks.push(TaskWithSubtasks {
            subtasks: subtasks_for_task(conn, task.id)?,
            task,
        });
    }

    Ok(tasks)
}

/// The subtasks already recorded against a task, most recently used first.
///
/// Subtasks are shared now, so this reads the task's history of use rather than
/// a set of subtasks belonging to it.
fn subtasks_for_task(conn: &Connection, task_id: i64) -> Result<Vec<Subtask>, TrackerError> {
    let mut stmt = conn.prepare(
        "
        SELECT s.id, s.name, s.archived_at, s.created_at, MAX(e.started_at) AS last_used
        FROM time_entries e
        JOIN subtasks s ON s.id = e.subtask_id
        WHERE e.task_id = ?1
        GROUP BY s.id
        ORDER BY last_used DESC
        ",
    )?;
    let rows = stmt.query_map(params![task_id], |row| {
        Ok(Subtask {
            id: row.get(0)?,
            name: row.get(1)?,
            archived_at: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;

    collect_rows(rows)
}

/// Every subtask, with how much time has been recorded against it.
fn list_subtasks_inner(conn: &Connection) -> Result<Vec<SubtaskView>, TrackerError> {
    let mut views: Vec<SubtaskView> = all_subtasks(conn)?
        .into_iter()
        .map(|subtask| SubtaskView {
            id: subtask.id,
            name: subtask.name,
            archived_at: subtask.archived_at,
            created_at: subtask.created_at,
            entry_count: 0,
            total_seconds: 0,
        })
        .collect();

    for entry in report_entries_inner(conn, None)? {
        let Some(subtask_id) = entry.subtask_id else {
            continue;
        };
        if let Some(view) = views.iter_mut().find(|view| view.id == subtask_id) {
            view.entry_count += 1;
            view.total_seconds += entry.duration_seconds;
        }
    }

    Ok(views)
}

fn all_subtasks(conn: &Connection) -> Result<Vec<Subtask>, TrackerError> {
    let mut stmt = conn.prepare(
        "
        SELECT id, name, archived_at, created_at
        FROM subtasks
        ORDER BY name ASC
        ",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Subtask {
            id: row.get(0)?,
            name: row.get(1)?,
            archived_at: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;

    collect_rows(rows)
}

fn create_subtask_inner(
    state: &State<'_, AppState>,
    input: CreateSubtaskInput,
) -> Result<Vec<SubtaskView>, TrackerError> {
    let mut conn = state.connect()?;
    create_subtask_conn(&mut conn, input)
}

fn create_subtask_conn(
    conn: &mut Connection,
    input: CreateSubtaskInput,
) -> Result<Vec<SubtaskView>, TrackerError> {
    let tx = conn.transaction()?;
    upsert_subtask(&tx, &input.name, &now_string())?;
    tx.commit()?;

    list_subtasks_inner(conn)
}

fn rename_subtask_inner(
    state: &State<'_, AppState>,
    input: RenameSubtaskInput,
) -> Result<Vec<SubtaskView>, TrackerError> {
    let mut conn = state.connect()?;
    rename_subtask_conn(&mut conn, input)
}

/// Renames a subtask, merging it into an existing one when the new name is
/// already taken. Merging repoints that subtask's time entries, so no recorded
/// time is lost.
fn rename_subtask_conn(
    conn: &mut Connection,
    input: RenameSubtaskInput,
) -> Result<Vec<SubtaskView>, TrackerError> {
    let display_name = normalize_subtask_name(&input.name);
    let name_key = subtask_name_key(&display_name);
    if name_key.is_empty() {
        return Err(TrackerError::MissingSubtaskName);
    }

    {
        let tx = conn.transaction()?;
        let existing_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM subtasks WHERE name_key = ?1 AND id <> ?2",
                params![name_key, input.subtask_id],
                |row| row.get(0),
            )
            .optional()?;

        match existing_id {
            Some(target_id) => {
                tx.execute(
                    "UPDATE time_entries SET subtask_id = ?1 WHERE subtask_id = ?2",
                    params![target_id, input.subtask_id],
                )?;
                tx.execute(
                    "DELETE FROM subtasks WHERE id = ?1",
                    params![input.subtask_id],
                )?;
            }
            None => {
                tx.execute(
                    "UPDATE subtasks SET name = ?1, name_key = ?2 WHERE id = ?3",
                    params![display_name, name_key, input.subtask_id],
                )?;
            }
        }

        tx.commit()?;
    }

    list_subtasks_inner(conn)
}

fn set_subtask_archived_inner(
    state: &State<'_, AppState>,
    input: SetSubtaskArchivedInput,
) -> Result<Vec<SubtaskView>, TrackerError> {
    let conn = state.connect()?;
    set_subtask_archived_conn(&conn, input)
}

/// Archives or restores a subtask. Archiving hides it from the pickers while
/// leaving its recorded time in reports.
fn set_subtask_archived_conn(
    conn: &Connection,
    input: SetSubtaskArchivedInput,
) -> Result<Vec<SubtaskView>, TrackerError> {
    let archived_at = input.archived.then(now_string);

    conn.execute(
        "UPDATE subtasks SET archived_at = ?1 WHERE id = ?2",
        params![archived_at, input.subtask_id],
    )?;

    list_subtasks_inner(conn)
}

fn latest_subtask_name_for_task(
    conn: &Connection,
    task_id: i64,
) -> Result<Option<String>, TrackerError> {
    conn.query_row(
        "
        SELECT s.name
        FROM time_entries e
        LEFT JOIN subtasks s ON s.id = e.subtask_id
        WHERE e.task_id = ?1
        ORDER BY e.started_at DESC
        LIMIT 1
        ",
        params![task_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(TrackerError::from)
}

fn upsert_task(tx: &Transaction<'_>, input: &TaskInput, now: &str) -> Result<Task, TrackerError> {
    let name = normalize_required(&input.name)?;
    let github_kind = normalize_optional(input.github_kind.clone());
    let github_reference = normalize_optional(input.github_reference.clone());
    let github_state = normalize_optional(input.github_state.clone());
    let github_checked_at = github_state.as_ref().map(|_| now.to_owned());

    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM tasks WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )
        .optional()?;

    let id = match existing_id {
        Some(id) => {
            tx.execute(
                "
                UPDATE tasks
                SET github_kind = ?1,
                    github_reference = ?2,
                    github_state = ?3,
                    github_checked_at = ?4,
                    closed_at = NULL,
                    updated_at = ?5
                WHERE id = ?6
                ",
                params![
                    github_kind,
                    github_reference,
                    github_state,
                    github_checked_at,
                    now,
                    id
                ],
            )?;
            id
        }
        None => {
            tx.execute(
                "
                INSERT INTO tasks (
                    name, github_kind, github_reference, github_state,
                    github_checked_at, created_at, updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                ",
                params![
                    name,
                    github_kind,
                    github_reference,
                    github_state,
                    github_checked_at,
                    now
                ],
            )?;
            tx.last_insert_rowid()
        }
    };

    task_by_id(tx, id)
}

/// Finds or creates a shared subtask by name.
///
/// Names that differ only in case or whitespace resolve to the same subtask.
/// Naming an archived subtask brings it back, since it is clearly in use again.
fn upsert_subtask(tx: &Transaction<'_>, name: &str, now: &str) -> Result<Subtask, TrackerError> {
    let display_name = normalize_subtask_name(name);
    let name_key = subtask_name_key(&display_name);
    if name_key.is_empty() {
        return Err(TrackerError::MissingSubtaskName);
    }

    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM subtasks WHERE name_key = ?1",
            params![name_key],
            |row| row.get(0),
        )
        .optional()?;

    let id = match existing_id {
        Some(id) => {
            tx.execute(
                "UPDATE subtasks SET archived_at = NULL WHERE id = ?1",
                params![id],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO subtasks (name, name_key, created_at) VALUES (?1, ?2, ?3)",
                params![display_name, name_key, now],
            )?;
            tx.last_insert_rowid()
        }
    };

    subtask_by_id(tx, id)
}

fn stop_active_entries(tx: &Transaction<'_>, ended_at: &str) -> Result<(), TrackerError> {
    tx.execute(
        "UPDATE time_entries SET ended_at = ?1 WHERE ended_at IS NULL",
        params![ended_at],
    )?;
    Ok(())
}

fn task_by_id(tx: &Transaction<'_>, id: i64) -> Result<Task, TrackerError> {
    tx.query_row(
        "
        SELECT id, name, github_kind, github_reference, github_state,
               github_checked_at, closed_at, created_at, updated_at
        FROM tasks
        WHERE id = ?1
        ",
        params![id],
        |row| {
            Ok(Task {
                id: row.get(0)?,
                name: row.get(1)?,
                github_kind: row.get(2)?,
                github_reference: row.get(3)?,
                github_state: row.get(4)?,
                github_checked_at: row.get(5)?,
                closed_at: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        },
    )
    .map_err(TrackerError::from)
}

fn task_by_id_conn(conn: &Connection, id: i64) -> Result<Task, TrackerError> {
    conn.query_row(
        "
        SELECT id, name, github_kind, github_reference, github_state,
               github_checked_at, closed_at, created_at, updated_at
        FROM tasks
        WHERE id = ?1
        ",
        params![id],
        |row| {
            Ok(Task {
                id: row.get(0)?,
                name: row.get(1)?,
                github_kind: row.get(2)?,
                github_reference: row.get(3)?,
                github_state: row.get(4)?,
                github_checked_at: row.get(5)?,
                closed_at: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        },
    )
    .map_err(TrackerError::from)
}

fn subtask_by_id(tx: &Transaction<'_>, id: i64) -> Result<Subtask, TrackerError> {
    tx.query_row(
        "SELECT id, name, archived_at, created_at FROM subtasks WHERE id = ?1",
        params![id],
        |row| {
            Ok(Subtask {
                id: row.get(0)?,
                name: row.get(1)?,
                archived_at: row.get(2)?,
                created_at: row.get(3)?,
            })
        },
    )
    .map_err(TrackerError::from)
}

fn row_to_entry_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<TimeEntryView> {
    let started_at: String = row.get(7)?;
    let ended_at: Option<String> = row.get(8)?;

    Ok(TimeEntryView {
        id: row.get(0)?,
        task_id: row.get(1)?,
        task_name: row.get(2)?,
        subtask_id: row.get(3)?,
        subtask_name: row.get(4)?,
        github_kind: row.get(5)?,
        github_reference: row.get(6)?,
        duration_seconds: elapsed_seconds(&started_at, ended_at.as_deref()).unwrap_or_default(),
        started_at,
        ended_at,
        note: row.get(9)?,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, TrackerError> {
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

fn elapsed_seconds(started_at: &str, ended_at: Option<&str>) -> Result<i64, TrackerError> {
    let start = DateTime::parse_from_rfc3339(started_at)?.with_timezone(&Utc);
    let end = match ended_at {
        Some(value) => DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc),
        None => Utc::now(),
    };

    Ok((end - start).num_seconds().max(0))
}

fn elapsed_seconds_in_range(
    started_at: &str,
    ended_at: Option<&str>,
    range: &ReportRange,
) -> Result<i64, TrackerError> {
    let start = DateTime::parse_from_rfc3339(started_at)?.with_timezone(&Utc);
    let end = match ended_at {
        Some(value) => DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc),
        None => Utc::now(),
    };
    let clipped_start = start.max(range.start);
    let clipped_end = end.min(range.end);

    Ok((clipped_end - clipped_start).num_seconds().max(0))
}

fn report_range_for_period(period: Option<String>) -> Result<Option<ReportRange>, TrackerError> {
    let today = Local::now().date_naive();
    match report_dates_for_period(period.as_deref(), today) {
        Some((start, end)) => Ok(Some(ReportRange {
            start: local_day_start(start),
            end: local_day_start(end),
        })),
        None => Ok(None),
    }
}

fn report_dates_for_period(
    period: Option<&str>,
    today: NaiveDate,
) -> Option<(NaiveDate, NaiveDate)> {
    match period {
        Some("today") => Some((today, today.checked_add_days(Days::new(1)).unwrap_or(today))),
        Some("this_week") => {
            let start = iso_week_start(today);
            Some((start, start.checked_add_days(Days::new(7)).unwrap_or(start)))
        }
        Some("last_week") => {
            let end = iso_week_start(today);
            let start = end.checked_sub_days(Days::new(7)).unwrap_or(end);
            Some((start, end))
        }
        Some("this_month") => {
            let start = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?;
            let (next_year, next_month) = if today.month() == 12 {
                (today.year() + 1, 1)
            } else {
                (today.year(), today.month() + 1)
            };
            let end = NaiveDate::from_ymd_opt(next_year, next_month, 1)?;
            Some((start, end))
        }
        _ => None,
    }
}

fn iso_week_start(date: NaiveDate) -> NaiveDate {
    date.checked_sub_days(Days::new(date.weekday().num_days_from_monday().into()))
        .unwrap_or(date)
}

fn local_day_start(date: NaiveDate) -> DateTime<Utc> {
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .expect("midnight should be a valid local time");

    match Local.from_local_datetime(&midnight) {
        LocalResult::Single(value) | LocalResult::Ambiguous(value, _) => value.with_timezone(&Utc),
        LocalResult::None => Utc::now(),
    }
}

fn normalize_required(value: &str) -> Result<String, TrackerError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(TrackerError::MissingTaskName);
    }
    Ok(value.to_owned())
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// The stored form of a subtask name: trimmed, with runs of whitespace
/// collapsed to single spaces.
fn normalize_subtask_name(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The value a subtask name is deduplicated on. Two names that differ only in
/// case or whitespace share a key and therefore share a subtask.
fn subtask_name_key(value: &str) -> String {
    normalize_subtask_name(value).to_lowercase()
}

fn now_string() -> String {
    format_utc(Utc::now())
}

fn format_utc(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .expect("enable foreign keys");
        db::create_latest_schema(&conn).expect("create schema");
        conn
    }

    fn create_test_task(conn: &mut Connection, name: &str) -> Task {
        let tx = conn.transaction().expect("begin transaction");
        let task = upsert_task(
            &tx,
            &TaskInput {
                name: name.to_owned(),
                github_kind: None,
                github_reference: None,
                github_state: None,
            },
            &now_string(),
        )
        .expect("upsert task");
        tx.commit().expect("commit transaction");
        task
    }

    #[test]
    fn close_task_input_accepts_frontend_camel_case() {
        let input: CloseTaskInput =
            serde_json::from_value(json!({ "taskId": 42 })).expect("deserialize close task input");

        assert_eq!(input.task_id, 42);
    }

    #[test]
    fn closing_task_removes_it_from_open_task_list() {
        let mut conn = memory_conn();
        let task = create_test_task(&mut conn, "Old task");

        assert_eq!(list_tasks_inner(&conn).expect("list tasks").len(), 1);

        close_task_conn(&mut conn, task.id).expect("close task");

        assert!(list_tasks_inner(&conn).expect("list tasks").is_empty());
        let closed_at: Option<String> = conn
            .query_row(
                "SELECT closed_at FROM tasks WHERE id = ?1",
                params![task.id],
                |row| row.get(0),
            )
            .expect("read closed_at");
        assert!(closed_at.is_some());
    }

    #[test]
    fn closing_task_stops_active_timer_for_that_task() {
        let mut conn = memory_conn();
        let task = create_test_task(&mut conn, "Running task");
        conn.execute(
            "INSERT INTO time_entries (task_id, started_at) VALUES (?1, ?2)",
            params![task.id, now_string()],
        )
        .expect("insert active timer");

        close_task_conn(&mut conn, task.id).expect("close task");

        let ended_at: Option<String> = conn
            .query_row(
                "SELECT ended_at FROM time_entries WHERE task_id = ?1",
                params![task.id],
                |row| row.get(0),
            )
            .expect("read ended_at");
        assert!(ended_at.is_some());
    }

    fn upsert_test_subtask(conn: &mut Connection, name: &str) -> Subtask {
        let tx = conn.transaction().expect("begin transaction");
        let subtask = upsert_subtask(&tx, name, &now_string()).expect("upsert subtask");
        tx.commit().expect("commit transaction");
        subtask
    }

    /// Records a finished entry of `minutes` against a task and optional subtask.
    fn insert_test_entry(
        conn: &Connection,
        task_id: i64,
        subtask_id: Option<i64>,
        minutes: i64,
    ) -> i64 {
        let started_at = Utc::now() - ChronoDuration::minutes(minutes);
        conn.execute(
            "
            INSERT INTO time_entries (task_id, subtask_id, started_at, ended_at)
            VALUES (?1, ?2, ?3, ?4)
            ",
            params![
                task_id,
                subtask_id,
                format_utc(started_at),
                format_utc(Utc::now())
            ],
        )
        .expect("insert time entry");
        conn.last_insert_rowid()
    }

    fn subtask_view(views: &[SubtaskView], name: &str) -> SubtaskView {
        views
            .iter()
            .find(|view| view.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("expected a subtask named {name}"))
    }

    #[test]
    fn subtask_names_are_shared_between_tasks() {
        let mut conn = memory_conn();
        create_test_task(&mut conn, "First task");
        create_test_task(&mut conn, "Second task");

        let first = upsert_test_subtask(&mut conn, "Review");
        let second = upsert_test_subtask(&mut conn, "Review");

        assert_eq!(
            first.id, second.id,
            "the same name should resolve to one shared subtask"
        );
        assert_eq!(all_subtasks(&conn).expect("list subtasks").len(), 1);
    }

    #[test]
    fn subtask_names_ignore_case_and_surrounding_whitespace() {
        let mut conn = memory_conn();

        let canonical = upsert_test_subtask(&mut conn, "Code review");

        for variant in ["code review", "  Code   review  ", "CODE REVIEW"] {
            let subtask = upsert_test_subtask(&mut conn, variant);
            assert_eq!(subtask.id, canonical.id, "{variant} should merge");
        }

        let subtasks = all_subtasks(&conn).expect("list subtasks");
        assert_eq!(subtasks.len(), 1);
        assert_eq!(
            subtasks[0].name, "Code review",
            "the first spelling should be kept"
        );
    }

    #[test]
    fn blank_subtask_names_are_rejected() {
        let mut conn = memory_conn();
        let tx = conn.transaction().expect("begin transaction");

        let error = upsert_subtask(&tx, "   ", &now_string());

        assert!(matches!(error, Err(TrackerError::MissingSubtaskName)));
    }

    #[test]
    fn naming_an_archived_subtask_brings_it_back() {
        let mut conn = memory_conn();
        let subtask = upsert_test_subtask(&mut conn, "Deploy");
        set_subtask_archived_conn(
            &conn,
            SetSubtaskArchivedInput {
                subtask_id: subtask.id,
                archived: true,
            },
        )
        .expect("archive subtask");

        let revived = upsert_test_subtask(&mut conn, "deploy");

        assert_eq!(revived.id, subtask.id);
        assert!(revived.archived_at.is_none());
    }

    #[test]
    fn archiving_a_subtask_keeps_its_recorded_time() {
        let mut conn = memory_conn();
        let task = create_test_task(&mut conn, "First task");
        let subtask = upsert_test_subtask(&mut conn, "Deploy");
        insert_test_entry(&conn, task.id, Some(subtask.id), 30);

        let views = set_subtask_archived_conn(
            &conn,
            SetSubtaskArchivedInput {
                subtask_id: subtask.id,
                archived: true,
            },
        )
        .expect("archive subtask");

        let view = subtask_view(&views, "Deploy");
        assert!(view.archived_at.is_some());
        assert_eq!(view.entry_count, 1);
        assert!(view.total_seconds >= 1800);
    }

    #[test]
    fn renaming_a_subtask_keeps_its_entries() {
        let mut conn = memory_conn();
        let task = create_test_task(&mut conn, "First task");
        let subtask = upsert_test_subtask(&mut conn, "Reviewing");
        let entry_id = insert_test_entry(&conn, task.id, Some(subtask.id), 15);

        let views = rename_subtask_conn(
            &mut conn,
            RenameSubtaskInput {
                subtask_id: subtask.id,
                name: "Code review".to_owned(),
            },
        )
        .expect("rename subtask");

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].name, "Code review");
        assert_eq!(
            entry_view_by_id(&conn, entry_id)
                .expect("read entry")
                .subtask_name
                .as_deref(),
            Some("Code review")
        );
    }

    #[test]
    fn renaming_onto_an_existing_name_merges_the_two_subtasks() {
        let mut conn = memory_conn();
        let task = create_test_task(&mut conn, "First task");
        let review = upsert_test_subtask(&mut conn, "Review");
        let reviewing = upsert_test_subtask(&mut conn, "Reviewing");
        let review_entry = insert_test_entry(&conn, task.id, Some(review.id), 20);
        let reviewing_entry = insert_test_entry(&conn, task.id, Some(reviewing.id), 40);

        let views = rename_subtask_conn(
            &mut conn,
            RenameSubtaskInput {
                subtask_id: reviewing.id,
                name: "review".to_owned(),
            },
        )
        .expect("rename subtask");

        assert_eq!(views.len(), 1, "the two subtasks should have merged");
        let view = subtask_view(&views, "Review");
        assert_eq!(view.id, review.id);
        assert_eq!(view.entry_count, 2, "both entries should have moved across");

        for entry_id in [review_entry, reviewing_entry] {
            assert_eq!(
                entry_view_by_id(&conn, entry_id)
                    .expect("read entry")
                    .subtask_id,
                Some(review.id)
            );
        }
    }

    #[test]
    fn creating_an_existing_subtask_does_not_duplicate_it() {
        let mut conn = memory_conn();

        create_subtask_conn(
            &mut conn,
            CreateSubtaskInput {
                name: "Review".to_owned(),
            },
        )
        .expect("create subtask");
        let views = create_subtask_conn(
            &mut conn,
            CreateSubtaskInput {
                name: " review ".to_owned(),
            },
        )
        .expect("create subtask again");

        assert_eq!(views.len(), 1);
    }

    #[test]
    fn subtask_report_totals_time_across_every_task() {
        let mut conn = memory_conn();
        let first = create_test_task(&mut conn, "First task");
        let second = create_test_task(&mut conn, "Second task");
        let review = upsert_test_subtask(&mut conn, "Review");
        let deploy = upsert_test_subtask(&mut conn, "Deploy");
        insert_test_entry(&conn, first.id, Some(review.id), 30);
        insert_test_entry(&conn, second.id, Some(review.id), 60);
        insert_test_entry(&conn, first.id, Some(deploy.id), 10);
        insert_test_entry(&conn, first.id, None, 5);

        let rows = summary_by_subtask_inner(&conn, None).expect("subtask summary");

        assert_eq!(rows.len(), 3, "two subtasks plus the unlabelled entry");
        let review_row = &rows[0];
        assert_eq!(review_row.subtask_name.as_deref(), Some("Review"));
        assert_eq!(review_row.entry_count, 2);
        assert_eq!(review_row.task_count, 2, "the subtask spans both tasks");
        assert!(review_row.total_seconds >= 5400);

        let unlabelled = rows
            .iter()
            .find(|row| row.subtask_id.is_none())
            .expect("a row for entries without a subtask");
        assert_eq!(unlabelled.entry_count, 1);
    }

    #[test]
    fn task_subtask_report_still_separates_the_same_subtask_per_task() {
        let mut conn = memory_conn();
        let first = create_test_task(&mut conn, "First task");
        let second = create_test_task(&mut conn, "Second task");
        let review = upsert_test_subtask(&mut conn, "Review");
        insert_test_entry(&conn, first.id, Some(review.id), 30);
        insert_test_entry(&conn, second.id, Some(review.id), 60);

        let rows = summary_by_task_and_subtask_inner(&conn, None).expect("task summary");

        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| row.subtask_name.as_deref() == Some("Review"))
        );
    }

    #[test]
    fn task_subtask_suggestions_list_recently_used_subtasks_first() {
        let mut conn = memory_conn();
        let task = create_test_task(&mut conn, "First task");
        let other = create_test_task(&mut conn, "Second task");
        let review = upsert_test_subtask(&mut conn, "Review");
        let deploy = upsert_test_subtask(&mut conn, "Deploy");
        let unused = upsert_test_subtask(&mut conn, "Unused here");
        insert_test_entry(&conn, task.id, Some(review.id), 90);
        insert_test_entry(&conn, task.id, Some(deploy.id), 10);
        insert_test_entry(&conn, other.id, Some(unused.id), 10);

        let subtasks = subtasks_for_task(&conn, task.id).expect("task subtasks");

        assert_eq!(
            subtasks
                .iter()
                .map(|subtask| subtask.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Deploy", "Review"],
            "most recently used first, and only this task's subtasks"
        );
    }

    #[test]
    fn report_dates_use_iso_week_boundaries() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).expect("test date");

        assert_eq!(
            report_dates_for_period(Some("this_week"), today),
            Some((
                NaiveDate::from_ymd_opt(2025, 12, 29).expect("week start"),
                NaiveDate::from_ymd_opt(2026, 1, 5).expect("week end")
            ))
        );
        assert_eq!(
            report_dates_for_period(Some("last_week"), today),
            Some((
                NaiveDate::from_ymd_opt(2025, 12, 22).expect("last week start"),
                NaiveDate::from_ymd_opt(2025, 12, 29).expect("last week end")
            ))
        );
    }

    #[test]
    fn report_dates_use_calendar_month_boundaries() {
        let today = NaiveDate::from_ymd_opt(2026, 12, 15).expect("test date");

        assert_eq!(
            report_dates_for_period(Some("this_month"), today),
            Some((
                NaiveDate::from_ymd_opt(2026, 12, 1).expect("month start"),
                NaiveDate::from_ymd_opt(2027, 1, 1).expect("month end")
            ))
        );
    }
}

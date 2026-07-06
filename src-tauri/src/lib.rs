use chrono::{DateTime, Days, Local, LocalResult, NaiveDate, SecondsFormat, TimeZone, Utc};
use keyring::v1::{Entry, Error as KeyringError};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tauri::menu::{Menu, MenuBuilder, SubmenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use thiserror::Error;

const TRAY_ID: &str = "tracker-tray";
const TRAY_TASK_PREFIX: &str = "start-task:";
const MENU_GITHUB_TOKEN_ID: &str = "github-token-settings";
const MENU_SHOW_ID: &str = "show-tracker";
const GITHUB_KEYCHAIN_SERVICE: &str = "dev.local.tracker";
const GITHUB_KEYCHAIN_USER: &str = "github-token";

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
}

impl AppState {
    fn connect(&self) -> Result<Connection, TrackerError> {
        let conn = Connection::open(&self.db_path)?;
        configure_database(&conn)?;
        Ok(conn)
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
    #[error("application data directory is not available")]
    MissingDataDir,
    #[error("tauri error: {0}")]
    Tauri(#[from] tauri::Error),
    #[error("github request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("keychain error: {0}")]
    Keychain(#[from] KeyringError),
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
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Subtask {
    id: i64,
    task_id: i64,
    name: String,
    created_at: String,
}

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
fn list_tasks(state: State<'_, AppState>) -> Result<Vec<TaskWithSubtasks>, String> {
    let conn = state.connect().map_err(String::from)?;
    list_tasks_inner(&conn).map_err(String::from)
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
fn start_timer(
    state: State<'_, AppState>,
    input: StartTimerInput,
    app: AppHandle,
) -> Result<ActiveTimer, String> {
    let timer = start_timer_inner(&state, input).map_err(String::from)?;
    let _ = refresh_tray_menu(&app);
    let _ = app.emit("timer-updated", ());
    Ok(timer)
}

#[tauri::command]
fn stop_timer(state: State<'_, AppState>, app: AppHandle) -> Result<Option<TimeEntryView>, String> {
    let stopped = stop_timer_inner(&state).map_err(String::from)?;
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
fn summary_by_subtask(
    state: State<'_, AppState>,
    period: Option<String>,
) -> Result<Vec<SummaryRow>, String> {
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|_| TrackerError::MissingDataDir)?;
            std::fs::create_dir_all(&data_dir)?;

            let state = AppState {
                db_path: data_dir.join("tracker.sqlite3"),
            };
            configure_database(&state.connect()?)?;
            app.manage(state);
            build_app_menu(app.handle())?;
            build_tray(app.handle())?;
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
            list_tasks,
            create_task,
            start_timer,
            stop_timer,
            get_active_timer,
            recent_entries,
            update_time_entry_subtask,
            summary_by_task,
            summary_by_subtask,
            search_github_references,
            get_github_token,
            set_github_token
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
    let mut builder = TrayIconBuilder::with_id(TRAY_ID).menu(&menu);
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
                    let _ = app.emit("timer-updated", ());
                }
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

fn build_tray_menu(app: &AppHandle) -> Result<Menu<tauri::Wry>, TrackerError> {
    let state = app.state::<AppState>();
    let conn = state.connect()?;
    let tasks = list_tasks_inner(&conn)?;

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

fn refresh_tray_menu(app: &AppHandle) -> Result<(), TrackerError> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(build_tray_menu(app)?))?;
    }
    Ok(())
}

fn tray_label(value: &str) -> String {
    const MAX_CHARS: usize = 42;
    if value.chars().count() <= MAX_CHARS {
        return value.to_owned();
    }

    let mut label = value.chars().take(MAX_CHARS - 1).collect::<String>();
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

fn configure_database(conn: &Connection) -> Result<(), TrackerError> {
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS tasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            github_kind TEXT,
            github_reference TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS subtasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(task_id, name),
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS time_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id INTEGER NOT NULL,
            subtask_id INTEGER,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            note TEXT,
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,
            FOREIGN KEY(subtask_id) REFERENCES subtasks(id) ON DELETE SET NULL
        );

        CREATE INDEX IF NOT EXISTS idx_time_entries_active
            ON time_entries(ended_at)
            WHERE ended_at IS NULL;

        CREATE INDEX IF NOT EXISTS idx_time_entries_started_at
            ON time_entries(started_at);
        ",
    )?;

    Ok(())
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
        Some(name) => Some(upsert_subtask(&tx, task.id, &name, &now)?),
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
                   t.id, t.name, t.github_kind, t.github_reference, t.created_at, t.updated_at,
                   s.id, s.task_id, s.name, s.created_at
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
                let subtask_id: Option<i64> = row.get(9)?;
                let subtask = match subtask_id {
                    Some(id) => Some(Subtask {
                        id,
                        task_id: row.get(10)?,
                        name: row.get(11)?,
                        created_at: row.get(12)?,
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
                        created_at: row.get(7)?,
                        updated_at: row.get(8)?,
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
        let task_id: i64 = tx.query_row(
            "SELECT task_id FROM time_entries WHERE id = ?1",
            params![entry_id],
            |row| row.get(0),
        )?;
        let now = now_string();
        let subtask_id = match normalize_optional(input.subtask_name) {
            Some(name) => Some(upsert_subtask(&tx, task_id, &name, &now)?.id),
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

fn summary_by_subtask_inner(
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
        SELECT id, name, github_kind, github_reference, created_at, updated_at
        FROM tasks
        ORDER BY updated_at DESC, name ASC
        ",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Task {
            id: row.get(0)?,
            name: row.get(1)?,
            github_kind: row.get(2)?,
            github_reference: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
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

fn subtasks_for_task(conn: &Connection, task_id: i64) -> Result<Vec<Subtask>, TrackerError> {
    let mut stmt = conn.prepare(
        "
        SELECT id, task_id, name, created_at
        FROM subtasks
        WHERE task_id = ?1
        ORDER BY name ASC
        ",
    )?;
    let rows = stmt.query_map(params![task_id], |row| {
        Ok(Subtask {
            id: row.get(0)?,
            task_id: row.get(1)?,
            name: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;

    collect_rows(rows)
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
                SET github_kind = ?1, github_reference = ?2, updated_at = ?3
                WHERE id = ?4
                ",
                params![github_kind, github_reference, now, id],
            )?;
            id
        }
        None => {
            tx.execute(
                "
                INSERT INTO tasks (name, github_kind, github_reference, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?4)
                ",
                params![name, github_kind, github_reference, now],
            )?;
            tx.last_insert_rowid()
        }
    };

    task_by_id(tx, id)
}

fn upsert_subtask(
    tx: &Transaction<'_>,
    task_id: i64,
    name: &str,
    now: &str,
) -> Result<Subtask, TrackerError> {
    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM subtasks WHERE task_id = ?1 AND name = ?2",
            params![task_id, name],
            |row| row.get(0),
        )
        .optional()?;

    let id = match existing_id {
        Some(id) => id,
        None => {
            tx.execute(
                "INSERT INTO subtasks (task_id, name, created_at) VALUES (?1, ?2, ?3)",
                params![task_id, name, now],
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
        SELECT id, name, github_kind, github_reference, created_at, updated_at
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
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        },
    )
    .map_err(TrackerError::from)
}

fn task_by_id_conn(conn: &Connection, id: i64) -> Result<Task, TrackerError> {
    conn.query_row(
        "
        SELECT id, name, github_kind, github_reference, created_at, updated_at
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
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        },
    )
    .map_err(TrackerError::from)
}

fn subtask_by_id(tx: &Transaction<'_>, id: i64) -> Result<Subtask, TrackerError> {
    tx.query_row(
        "SELECT id, task_id, name, created_at FROM subtasks WHERE id = ?1",
        params![id],
        |row| {
            Ok(Subtask {
                id: row.get(0)?,
                task_id: row.get(1)?,
                name: row.get(2)?,
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
    match period.as_deref() {
        Some("today") => {
            let today = Local::now().date_naive();
            let tomorrow = today.checked_add_days(Days::new(1)).unwrap_or(today);

            Ok(Some(ReportRange {
                start: local_day_start(today),
                end: local_day_start(tomorrow),
            }))
        }
        _ => Ok(None),
    }
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

fn now_string() -> String {
    format_utc(Utc::now())
}

fn format_utc(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

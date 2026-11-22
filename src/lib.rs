use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, ValueEnum};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

mod token;

pub use token::Token;

/// A put.io file type, as reported by the API and accepted by `--file-type`
/// and `--delete-type`.
///
/// `FOLDER` is deliberately absent: deleting a folder on put.io deletes
/// everything inside it, so it is not something either flag should accept.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    #[value(name = "VIDEO")]
    Video,
    #[value(name = "AUDIO")]
    Audio,
    #[value(name = "IMAGE")]
    Image,
    #[value(name = "ARCHIVE")]
    Archive,
    #[value(name = "PDF")]
    Pdf,
    #[value(name = "TEXT")]
    Text,
    #[value(name = "FILE")]
    File,
}

impl FileType {
    /// The name the put.io API uses for this type.
    pub fn as_str(self) -> &'static str {
        match self {
            FileType::Video => "VIDEO",
            FileType::Audio => "AUDIO",
            FileType::Image => "IMAGE",
            FileType::Archive => "ARCHIVE",
            FileType::Pdf => "PDF",
            FileType::Text => "TEXT",
            FileType::File => "FILE",
        }
    }
}

/// Command-line configuration for the put.io tool.
///
/// `parent_id` is required and positional. The OAuth token is not a flag; it
/// is found by [`Config::resolve_token`].
#[derive(Parser, Debug)]
// long_about = None keeps the rustdoc above out of --help, which is written for
// someone running the binary, not reading the crate.
#[command(about = "List, download, and clean up files on put.io", long_about = None)]
pub struct Config {
    /// Parent folder ID on put.io
    #[arg(required_unless_present_any = ["login", "logout"])]
    pub parent_id: Option<i64>,

    /// Maximum number of download URLs to output
    #[arg(long)]
    pub max_display: Option<u32>,

    /// Results per API page
    #[arg(long, default_value_t = 1)]
    pub per_page: i8,

    /// Filter download mode to a single file type.
    ///
    /// Mutually exclusive with `--delete-type`.
    #[arg(long, value_enum, ignore_case = true)]
    pub file_type: Option<FileType>,

    /// File types to delete from put.io (repeatable). Activates cleanup mode.
    ///
    /// Mutually exclusive with `--file-type`.
    #[arg(long, value_enum, ignore_case = true, conflicts_with = "file_type")]
    pub delete_type: Vec<FileType>,

    /// Shell command printing the put.io OAuth token, e.g. 'pass putio/token'
    #[arg(long, env = "PUTIO_TOKEN_COMMAND", value_name = "CMD")]
    pub token_command: Option<String>,

    /// File holding the put.io OAuth token, mode 0600
    #[arg(long, env = "PUTIO_TOKEN_FILE", value_name = "PATH")]
    pub token_file: Option<PathBuf>,

    /// Store the put.io OAuth token read from stdin in the OS keyring, then exit
    #[arg(long, conflicts_with_all = ["logout", "parent_id"])]
    pub login: bool,

    /// Delete the put.io OAuth token from the OS keyring, then exit
    #[arg(long, conflicts_with = "parent_id")]
    pub logout: bool,

    #[arg(skip)]
    pub oauth_token: Token,
}

impl Config {
    fn folder_id(&self) -> i64 {
        self.parent_id
            .expect("clap requires parent_id outside --login/--logout")
    }

    /// Find the OAuth token. See [`token::resolve`] for the sources and their
    /// order.
    ///
    /// # Errors
    ///
    /// Returns an error if no source yields a usable token, or if a source the
    /// caller named explicitly fails.
    pub fn resolve_token(&mut self) -> Result<()> {
        self.oauth_token =
            token::resolve(self.token_command.as_deref(), self.token_file.as_deref())?;
        Ok(())
    }

    fn ensure_token(&mut self) -> Result<()> {
        if self.oauth_token.is_empty() {
            self.resolve_token()?;
        }
        Ok(())
    }
}

struct Client {
    oauth_token: Token,
}

impl Client {
    pub fn new(oauth_token: &Token) -> Client {
        Client {
            oauth_token: oauth_token.clone(),
        }
    }

    pub fn get(&self, url: &str) -> Result<reqwest::blocking::RequestBuilder> {
        let client = reqwest::blocking::Client::new();
        let headers = get_authorization_header(&self.oauth_token)?;

        Ok(client.get(url).headers(headers))
    }

    pub fn post(&self, url: &str) -> Result<reqwest::blocking::RequestBuilder> {
        let client = reqwest::blocking::Client::new();
        let headers = get_authorization_header(&self.oauth_token)?;

        Ok(client.post(url).headers(headers))
    }
}

/// Number of additional attempts after the first try fails transiently.
const MAX_RETRIES: u32 = 3;
/// Base backoff between retries; doubled each attempt (0.5s, 1s, 2s).
const RETRY_BASE_DELAY_MS: u64 = 500;

/// Send `builder`, retrying on transient transport errors (e.g. the
/// intermittent TLS "bad record mac" failures seen against api.put.io) and on
/// retryable HTTP statuses (429 throttling and 5xx). Uses exponential backoff.
///
/// Non-retryable responses (success, 4xx other than 429) are returned as-is so
/// callers keep their existing status handling.
fn send_with_retry(
    builder: reqwest::blocking::RequestBuilder,
) -> Result<reqwest::blocking::Response> {
    let mut attempt: u32 = 0;
    loop {
        let attempt_builder = builder
            .try_clone()
            .ok_or_else(|| anyhow!("request is not cloneable, cannot retry"))?;
        match attempt_builder.send() {
            Ok(response) => {
                let status = response.status();
                let retryable =
                    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                if retryable && attempt < MAX_RETRIES {
                    let delay = RETRY_BASE_DELAY_MS << attempt;
                    debug!(
                        "Retryable HTTP {} (retry {}/{}), backing off {}ms",
                        status,
                        attempt + 1,
                        MAX_RETRIES,
                        delay
                    );
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    attempt += 1;
                    continue;
                }
                return Ok(response);
            }
            Err(e) if attempt < MAX_RETRIES => {
                let delay = RETRY_BASE_DELAY_MS << attempt;
                debug!(
                    "Transport error '{}' (retry {}/{}), backing off {}ms",
                    e,
                    attempt + 1,
                    MAX_RETRIES,
                    delay
                );
                std::thread::sleep(std::time::Duration::from_millis(delay));
                attempt += 1;
            }
            Err(e) => {
                return Err(e).with_context(|| format!("giving up after {} retries", MAX_RETRIES))
            }
        }
    }
}

/// Represents the Put.io API error response body.
///
/// Returned by the API when a request fails (e.g., file not found).
/// Example: `{"error_id":null,"error_message":"File not found.","error_type":"FileNotFound",...}`
// Fields are populated by serde deserialization and used in tests; not read in non-test code paths.
#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct ApiError {
    error_type: Option<String>,
    error_message: Option<String>,
    status_code: Option<i32>,
}

#[derive(Serialize, Deserialize)]
struct FileURL {
    url: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct File {
    content_type: String,
    file_type: String,
    id: i64,
    name: String,
    parent_id: i64,
    size: i64,
}

#[derive(Serialize, Deserialize, Debug)]
struct Files {
    files: Vec<File>,
    status: String,
    total: i32,
}

fn get_access_token_header(access_token: &str) -> String {
    format!("Token {}", access_token)
}

fn get_authorization_header(oauth_token: &Token) -> Result<reqwest::header::HeaderMap> {
    let token = get_access_token_header(oauth_token.expose());
    let mut headers = reqwest::header::HeaderMap::with_capacity(1);
    let mut value = reqwest::header::HeaderValue::from_str(&token)
        .context("the put.io token cannot be sent in an HTTP header")?;
    value.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, value);

    Ok(headers)
}

fn list_dir(dir: &Path) -> Result<Vec<String>> {
    let mut all_files: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            let path = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid filename encoding")
            })?;
            all_files.push(String::from(path));
        }
    }
    for file in &all_files {
        let aria2_extension = ".aria2";
        if !file.ends_with(&aria2_extension) {
            let aria2_file = format!("{}.aria2", file);
            if file != &aria2_file && !all_files.iter().any(|x| x == &aria2_file) {
                files.push(file.clone());
            }
        }
    }
    Ok(files)
}

/// Delete a file or folder from put.io.
///
/// A refused delete is an error, because callers act on success: `run_download`
/// moves the local copy away once put.io confirms its copy is gone. A 404, or a
/// `FileNotFound` body under any status, counts as deleted, so a delete retried
/// after an earlier run removed the file does not fail forever.
fn delete_file(client: &Client, file_id: i64) -> Result<()> {
    let url = "https://api.put.io/v2/files/delete".to_string();
    let body = json!({ "file_ids": file_id }).to_string();
    let response = send_with_retry(
        client
            .post(&url)?
            .header(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/json"),
            )
            .body(body),
    )?;

    let status = response.status();
    if status.is_success() {
        debug!("Deleted file_id={}", file_id);
        return Ok(());
    }
    delete_outcome(file_id, status, response.json::<ApiError>().ok())
}

/// What a non-success response to a delete means. Separate from the request so
/// it can be tested without an HTTP server.
fn delete_outcome(
    file_id: i64,
    status: reqwest::StatusCode,
    error: Option<ApiError>,
) -> Result<()> {
    let already_gone = status == reqwest::StatusCode::NOT_FOUND
        || error
            .as_ref()
            .and_then(|e| e.error_type.as_deref())
            .is_some_and(|t| t == "FileNotFound");
    if already_gone {
        debug!(
            "file_id={} is already absent from put.io (HTTP {}), counting as deleted",
            file_id, status
        );
        return Ok(());
    }

    let error_msg = error
        .and_then(|e| e.error_message)
        .unwrap_or_else(|| format!("HTTP {}", status));
    bail!("Error deleting file_id={}: {}", file_id, error_msg)
}

/// The `error_message` put.io returned for a failed request, or the status
/// itself when the body does not parse as an error.
fn api_error_message(response: reqwest::blocking::Response, status: reqwest::StatusCode) -> String {
    response
        .json::<ApiError>()
        .ok()
        .and_then(|e| e.error_message)
        .unwrap_or_else(|| format!("HTTP {}", status))
}

/// Fetch one page of files from put.io for a given `parent_id`, optionally
/// filtered by `file_type` (e.g. `Some("VIDEO")`, `Some("FOLDER")`).
fn fetch_files_page(
    client: &Client,
    parent_id: i64,
    per_page: i8,
    file_type: Option<&str>,
) -> Result<Vec<File>> {
    let mut url = format!(
        "https://api.put.io/v2/files/list?parent_id={}&per_page={}",
        parent_id, per_page
    );
    if let Some(ft) = file_type {
        url.push_str("&file_type=");
        url.push_str(ft);
    }
    debug!(
        "Listing files for parent_id={} per_page={} file_type={:?}",
        parent_id, per_page, file_type
    );
    let response = send_with_retry(client.get(&url)?)?;
    if response.status().is_success() {
        let response_files: Files = response.json()?;
        debug!(
            "Got {} files (status: {})",
            response_files.files.len(),
            response_files.status
        );
        Ok(response_files.files)
    } else {
        let status = response.status();
        debug!(
            "files_list failed for parent_id={}: HTTP {}",
            parent_id, status
        );
        bail!(
            "Error listing files for parent_id={}: {}",
            parent_id,
            api_error_message(response, status)
        )
    }
}

/// Recursively list files under `parent_id`, optionally filtered by `file_type`.
///
/// When `file_type` is `Some(t)`, the API returns only files of that type, so
/// folders are fetched separately to drive recursion. When `None`, a single
/// unfiltered call is used and folders are discovered inline.
fn files_list(
    client: &Client,
    parent_id: i64,
    per_page: i8,
    file_type: Option<&str>,
) -> Result<Vec<File>> {
    let mut files: Vec<File> = Vec::new();

    if let Some(ft) = file_type {
        // Fetch + recurse into subfolders.
        let folders = fetch_files_page(client, parent_id, per_page, Some("FOLDER"))?;
        for folder in folders {
            let sub = files_list(client, folder.id, per_page, Some(ft))?;
            files.extend(sub);
        }
        // Fetch the requested type.
        let matched = fetch_files_page(client, parent_id, per_page, Some(ft))?;
        files.extend(matched);
    } else {
        // Unfiltered: one call, recurse on FOLDER inline, return everything else.
        let page = fetch_files_page(client, parent_id, per_page, None)?;
        for file in page {
            if file.file_type == "FOLDER" {
                let sub = files_list(client, file.id, per_page, None)?;
                files.extend(sub);
            } else {
                files.push(file);
            }
        }
    }

    Ok(files)
}

fn file_url(client: &Client, file_id: i64) -> Result<String> {
    let url = format!("https://api.put.io/v2/files/{}/url", file_id);
    debug!("Fetching download URL for file_id={}", file_id);
    let response = send_with_retry(client.get(&url)?)?;
    if response.status().is_success() {
        let file_url: FileURL = response.json()?;
        debug!("Got download URL for file_id={}", file_id);
        Ok(file_url.url)
    } else {
        let status = response.status();
        debug!("file_url failed for file_id={}: HTTP {}", file_id, status);
        bail!(
            "Error fetching download url for file_id={}: {}",
            file_id,
            api_error_message(response, status)
        )
    }
}

/// Entry point. Handles the keyring flags, then dispatches to download or
/// cleanup mode based on `config`.
///
/// # Errors
///
/// Returns any error from token resolution, the put.io API, or local
/// filesystem operations.
pub fn run(mut config: Config) -> Result<()> {
    if config.login {
        return token::login();
    }
    if config.logout {
        return token::logout();
    }

    config.ensure_token()?;
    let client = Client::new(&config.oauth_token);
    if !config.delete_type.is_empty() {
        run_cleanup(&config, &client)
    } else {
        run_download(&config, &client)
    }
}

/// Download mode: list files (optionally filtered by `--file-type`), print
/// download URLs for files not yet on disk, and clean up files that have
/// already been downloaded locally.
fn run_download(config: &Config, client: &Client) -> Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let path = Path::new(".");
    let downloaded_files = list_dir(path)?;
    let file_type = config.file_type.map(FileType::as_str);

    let files = files_list(client, config.folder_id(), config.per_page, file_type)?;
    debug!("Total files to process: {}", files.len());
    let mut num_printed: u32 = 0;

    for file in files {
        debug!(
            "Processing file: {} (type: {}, id: {})",
            file.name, file.file_type, file.id
        );
        if !downloaded_files.iter().any(|x| x == &file.name) {
            if config.max_display.is_none_or(|max| num_printed < max) {
                // Only this arm needs a URL. A file already on disk is about
                // to be deleted from put.io, and one past `max_display` is not
                // being downloaded this run.
                let download_url = file_url(client, file.id)?;
                num_printed += 1;
                writeln!(handle, "{}", download_url)?;
            }
        } else {
            debug!("Deleting file with ID: {} (file: {})", file.id, file.name);
            // Strict: the rename below moves the local copy out of the way,
            // so a delete that did not take means a later run downloads the
            // file again. Failing here leaves both copies in place to retry.
            delete_file(client, file.id)?;
            delete_parent_if_empty(client, &file, config)?;
            let completed_dir = Path::new("completed");
            if !completed_dir.is_dir() {
                fs::create_dir(completed_dir)
                    .with_context(|| format!("creating {}", completed_dir.display()))?;
            }
            let completed_path = completed_dir.join(&file.name);
            fs::rename(&file.name, &completed_path)
                .with_context(|| format!("moving {} to {}", file.name, completed_path.display()))?;
        }
    }

    Ok(())
}

/// Cleanup mode: for each `--delete-type`, list matching files on put.io
/// and delete them. Empty parent folders are removed when safe.
fn run_cleanup(config: &Config, client: &Client) -> Result<()> {
    for delete_type in &config.delete_type {
        let type_name = delete_type.as_str();
        let files = files_list(client, config.folder_id(), config.per_page, Some(type_name))?;
        debug!("Cleanup: found {} files of type {}", files.len(), type_name);
        for file in files {
            debug!(
                "Deleting file with ID: {} (file: {}, type: {})",
                file.id, file.name, file.file_type
            );
            delete_file(client, file.id)?;
            delete_parent_if_empty(client, &file, config)?;
        }
    }
    Ok(())
}

/// Delete `file`'s parent folder if the file came from a subfolder that is now
/// empty of videos.
///
/// Best-effort, unlike the file delete that precedes it: that one has already
/// succeeded, so aborting here would leave the file gone from put.io with the
/// run unfinished. An empty folder left behind costs nothing and the next run
/// tries again.
fn delete_parent_if_empty(client: &Client, file: &File, config: &Config) -> Result<()> {
    if file.parent_id == config.folder_id()
        || !parent_safe_to_delete(client, file, config.per_page)?
    {
        return Ok(());
    }
    debug!(
        "Deleting empty parent directory with ID: {} (file: {})",
        file.parent_id, file.name
    );
    if let Err(e) = delete_file(client, file.parent_id) {
        warn!(
            "Could not delete parent directory {} (file: {}): {}",
            file.parent_id, file.name, e
        );
    }
    Ok(())
}

fn parent_safe_to_delete(client: &Client, file: &File, per_page: i8) -> Result<bool> {
    debug!(
        "Checking if parent {} is safe to delete (file: {})",
        file.parent_id, file.name
    );
    // Always ask for VIDEO, whatever the caller filtered on: the rule is
    // "keep folders that still hold videos", and a FILE or IMAGE filter
    // would hide every video sibling from this check.
    //
    // One more entry than the caller's per_page, so the listing has room for
    // the just-deleted file (put.io may still list it) and for at least one
    // survivor. At per_page=1 the check would otherwise be blind.
    let files = files_list(
        client,
        file.parent_id,
        per_page.saturating_add(1),
        Some("VIDEO"),
    )?;
    debug!("Found {} total files in parent directory", files.len());
    for file_in_dir in files {
        debug!(
            "  Checking file: {} (type: {}, id: {})",
            file_in_dir.name, file_in_dir.file_type, file_in_dir.id
        );
        if file_in_dir.file_type == "VIDEO" && file_in_dir.id != file.id {
            debug!(
                "Another VIDEO found ({}), parent NOT safe to delete",
                file_in_dir.name
            );
            return Ok(false);
        }
    }
    debug!("No other videos in parent, parent IS safe to delete");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that mutate the environment the token chain reads, so
    // cargo's parallel test runner can't race a set/remove against another
    // test's read.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Stops a test picking up a real token from the developer's environment.
    /// Caller holds `ENV_LOCK`.
    fn clear_token_env() {
        std::env::remove_var("PUTIO_TOKEN");
        std::env::remove_var("PUTIO_TOKEN_COMMAND");
        std::env::remove_var("PUTIO_TOKEN_FILE");
        std::env::set_var("XDG_CONFIG_HOME", "/nonexistent-putio-test-config");
    }

    #[test]
    fn config_missing_parent_id() {
        let result = Config::try_parse_from(["putio"]);
        assert!(result.is_err(), "Should fail when parent_id is missing");
    }

    #[test]
    fn config_resolve_token_no_source_lists_every_source() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        let mut config = Config::try_parse_from(["putio", "921234"]).expect("parse");
        let err = config.resolve_token().expect_err("no token is an error");
        let msg = err.to_string();
        assert!(msg.contains("--token-command"), "{}", msg);
        assert!(msg.contains("--token-file"), "{}", msg);
        assert!(msg.contains("PUTIO_TOKEN"), "{}", msg);
    }

    #[test]
    fn config_resolve_token_from_env() -> Result<(), String> {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        std::env::set_var("PUTIO_TOKEN", "test_token_from_env");
        let mut config = Config::try_parse_from(["putio", "921234"]).map_err(|e| e.to_string())?;
        let resolved = config.resolve_token();
        std::env::remove_var("PUTIO_TOKEN");
        resolved.map_err(|e| e.to_string())?;
        assert_eq!(config.oauth_token.expose(), "test_token_from_env");
        assert_eq!(config.parent_id, Some(921234));
        Ok(())
    }

    #[test]
    fn config_login_needs_no_parent_id() -> Result<(), String> {
        let config = Config::try_parse_from(["putio", "--login"]).map_err(|e| e.to_string())?;
        assert!(config.login);
        assert_eq!(config.parent_id, None);
        Ok(())
    }

    #[test]
    fn config_logout_needs_no_parent_id() -> Result<(), String> {
        let config = Config::try_parse_from(["putio", "--logout"]).map_err(|e| e.to_string())?;
        assert!(config.logout);
        Ok(())
    }

    #[test]
    fn config_login_and_logout_conflict() {
        let result = Config::try_parse_from(["putio", "--login", "--logout"]);
        assert!(result.is_err());
    }

    #[test]
    fn config_login_rejects_a_parent_id() {
        let result = Config::try_parse_from(["putio", "--login", "921234"]);
        assert!(result.is_err());
    }

    #[test]
    fn config_resolve_token_from_command() -> Result<(), String> {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        let mut config = Config::try_parse_from([
            "putio",
            "921234",
            "--token-command",
            "echo token_from_command",
        ])
        .map_err(|e| e.to_string())?;
        config.resolve_token().map_err(|e| e.to_string())?;
        assert_eq!(config.oauth_token.expose(), "token_from_command");
        Ok(())
    }

    #[test]
    fn config_resolve_token_failing_command_does_not_fall_back_to_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        std::env::set_var("PUTIO_TOKEN", "token_from_env");
        let mut config = Config::try_parse_from(["putio", "921234", "--token-command", "exit 3"])
            .expect("parse");
        let result = config.resolve_token();
        std::env::remove_var("PUTIO_TOKEN");
        let err = result.expect_err("a failing token command is an error");
        assert!(err.to_string().contains("exit status 3"), "{}", err);
    }

    #[test]
    fn config_resolve_token_command_beats_env() -> Result<(), String> {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        std::env::set_var("PUTIO_TOKEN", "token_from_env");
        let mut config = Config::try_parse_from([
            "putio",
            "921234",
            "--token-command",
            "echo token_from_command",
        ])
        .map_err(|e| e.to_string())?;
        let resolved = config.resolve_token();
        std::env::remove_var("PUTIO_TOKEN");
        resolved.map_err(|e| e.to_string())?;
        assert_eq!(config.oauth_token.expose(), "token_from_command");
        Ok(())
    }

    #[test]
    fn config_resolve_token_from_file() -> Result<(), String> {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        let path = write_token_file("from_file", "token_from_file\n", 0o600)?;
        let mut config = Config::try_parse_from([
            "putio",
            "921234",
            "--token-file",
            &path.display().to_string(),
        ])
        .map_err(|e| e.to_string())?;
        let resolved = config.resolve_token();
        let _ = fs::remove_file(&path);
        resolved.map_err(|e| e.to_string())?;
        assert_eq!(config.oauth_token.expose(), "token_from_file");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn config_resolve_token_rejects_a_world_readable_file() -> Result<(), String> {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        let path = write_token_file("world_readable", "token_from_file\n", 0o644)?;
        let mut config = Config::try_parse_from([
            "putio",
            "921234",
            "--token-file",
            &path.display().to_string(),
        ])
        .map_err(|e| e.to_string())?;
        let result = config.resolve_token();
        let _ = fs::remove_file(&path);
        let err = result.expect_err("a 0644 token file must be refused");
        assert!(err.to_string().contains("chmod 600"), "{}", err);
        Ok(())
    }

    #[test]
    fn config_resolve_token_from_the_default_file() -> Result<(), String> {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        let config_home = std::env::temp_dir().join(format!(
            "putio-test-xdg-{}-{}",
            std::process::id(),
            "default"
        ));
        let dir = config_home.join("putio");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("token");
        fs::write(&path, "token_from_default_file\n").map_err(|e| e.to_string())?;
        set_mode(&path, 0o600)?;
        std::env::set_var("XDG_CONFIG_HOME", &config_home);

        let mut config = Config::try_parse_from(["putio", "921234"]).map_err(|e| e.to_string())?;
        let resolved = config.resolve_token();
        let _ = fs::remove_dir_all(&config_home);
        resolved.map_err(|e| e.to_string())?;
        assert_eq!(config.oauth_token.expose(), "token_from_default_file");
        Ok(())
    }

    #[test]
    fn ensure_token_keeps_a_token_the_caller_already_set() -> Result<(), String> {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        std::env::set_var("PUTIO_TOKEN", "token_from_env");
        let mut config = Config::try_parse_from(["putio", "921234"]).map_err(|e| e.to_string())?;
        config.oauth_token =
            Token::parse("token_from_the_caller", "a test").map_err(|e| e.to_string())?;
        let ensured = config.ensure_token();
        std::env::remove_var("PUTIO_TOKEN");
        ensured.map_err(|e| e.to_string())?;
        assert_eq!(config.oauth_token.expose(), "token_from_the_caller");
        Ok(())
    }

    #[test]
    fn ensure_token_resolves_when_the_caller_set_none() -> Result<(), String> {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        std::env::set_var("PUTIO_TOKEN", "token_from_env");
        let mut config = Config::try_parse_from(["putio", "921234"]).map_err(|e| e.to_string())?;
        let ensured = config.ensure_token();
        std::env::remove_var("PUTIO_TOKEN");
        ensured.map_err(|e| e.to_string())?;
        assert_eq!(config.oauth_token.expose(), "token_from_env");
        Ok(())
    }

    #[test]
    fn config_debug_does_not_leak_the_token() -> Result<(), String> {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_token_env();
        std::env::set_var("PUTIO_TOKEN", "sup3rs3cret");
        let mut config = Config::try_parse_from(["putio", "921234"]).map_err(|e| e.to_string())?;
        let resolved = config.resolve_token();
        std::env::remove_var("PUTIO_TOKEN");
        resolved.map_err(|e| e.to_string())?;
        let debug = format!("{:?}", config);
        assert!(!debug.contains("sup3rs3cret"), "{}", debug);
        assert!(debug.contains("Token(***)"), "{}", debug);
        Ok(())
    }

    fn write_token_file(name: &str, contents: &str, mode: u32) -> Result<PathBuf, String> {
        let path =
            std::env::temp_dir().join(format!("putio-test-token-{}-{}", std::process::id(), name));
        fs::write(&path, contents).map_err(|e| e.to_string())?;
        set_mode(&path, mode)?;
        Ok(path)
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())
    }

    #[cfg(not(unix))]
    fn set_mode(_path: &Path, _mode: u32) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn config_file_type_and_delete_type_conflict() {
        let result = Config::try_parse_from([
            "putio",
            "921234",
            "--file-type",
            "VIDEO",
            "--delete-type",
            "ARCHIVE",
        ]);
        assert!(
            result.is_err(),
            "--file-type and --delete-type must be mutually exclusive"
        );
    }

    #[test]
    fn config_delete_type_repeatable() -> Result<(), String> {
        let config = Config::try_parse_from([
            "putio",
            "921234",
            "--delete-type",
            "ARCHIVE",
            "--delete-type",
            "IMAGE",
        ])
        .map_err(|e| e.to_string())?;
        assert_eq!(config.delete_type, vec![FileType::Archive, FileType::Image]);
        Ok(())
    }

    #[test]
    fn config_rejects_unknown_file_type() {
        let result = Config::try_parse_from(["putio", "921234", "--file-type", "VIDEOS"]);
        assert!(result.is_err(), "a typo must not be passed to the API");
    }

    /// Deleting a folder on put.io deletes everything inside it.
    #[test]
    fn config_rejects_folder_as_a_delete_type() {
        let result = Config::try_parse_from(["putio", "921234", "--delete-type", "FOLDER"]);
        assert!(result.is_err());
    }

    #[test]
    fn config_file_type_is_case_insensitive() -> Result<(), String> {
        let config = Config::try_parse_from(["putio", "921234", "--file-type", "video"])
            .map_err(|e| e.to_string())?;
        assert_eq!(config.file_type, Some(FileType::Video));
        Ok(())
    }

    fn api_error(error_type: Option<&str>, message: Option<&str>) -> ApiError {
        ApiError {
            error_type: error_type.map(String::from),
            error_message: message.map(String::from),
            status_code: None,
        }
    }

    /// A throttled delete must be an error: the caller moves the local copy
    /// away on success, and put.io still holds its own.
    #[test]
    fn delete_outcome_throttled_is_an_error() {
        let err = delete_outcome(
            42,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some(api_error(Some("Throttled"), Some("Too many requests."))),
        )
        .expect_err("a 429 must not count as deleted");
        assert!(
            err.to_string().contains("Too many requests."),
            "the put.io message belongs in the error: {}",
            err
        );
        assert!(err.to_string().contains("42"), "and the file id: {}", err);
    }

    #[test]
    fn delete_outcome_server_error_is_an_error() {
        assert!(delete_outcome(42, reqwest::StatusCode::INTERNAL_SERVER_ERROR, None).is_err());
    }

    /// A delete retried after an earlier run removed the file must not fail.
    #[test]
    fn delete_outcome_404_counts_as_deleted() {
        assert!(delete_outcome(42, reqwest::StatusCode::NOT_FOUND, None).is_ok());
    }

    #[test]
    fn delete_outcome_file_not_found_body_counts_as_deleted() {
        assert!(delete_outcome(
            42,
            reqwest::StatusCode::BAD_REQUEST,
            Some(api_error(Some("FileNotFound"), Some("File not found."))),
        )
        .is_ok());
    }

    /// An unparseable body must still fail, with the status as the message.
    #[test]
    fn delete_outcome_unreadable_body_still_fails_with_the_status() {
        let err = delete_outcome(42, reqwest::StatusCode::BAD_REQUEST, None)
            .expect_err("a 400 must not count as deleted");
        assert!(err.to_string().contains("400"), "{}", err);
    }

    #[test]
    fn api_error_deserialize_file_not_found() {
        let json = r#"{"error_id":null,"error_message":"File not found.","error_type":"FileNotFound","error_uri":"http://api.put.io/v2/docs","extra":{"file_id":789844},"status":"ERROR","status_code":404}"#;
        let api_error: ApiError = serde_json::from_str(json).unwrap();
        assert_eq!(api_error.error_type.as_deref(), Some("FileNotFound"));
        assert_eq!(api_error.error_message.as_deref(), Some("File not found."));
        assert_eq!(api_error.status_code, Some(404));
    }

    #[test]
    fn api_error_deserialize_minimal() {
        let json = r#"{"status":"ERROR"}"#;
        let api_error: ApiError = serde_json::from_str(json).unwrap();
        assert!(api_error.error_type.is_none());
        assert!(api_error.error_message.is_none());
        assert!(api_error.status_code.is_none());
    }

    #[test]
    fn file_url_deserialize_valid() {
        let json = r#"{"status":"OK","url":"https://example.com/download"}"#;
        let file_url: FileURL = serde_json::from_str(json).unwrap();
        assert_eq!(file_url.url, "https://example.com/download");
    }

    // The root folder has no parent, so put.io answers a parent_id=0 listing
    // with a parent object whose every field is null.
    #[test]
    fn files_deserialize_root_listing() {
        let json = r#"{"files":[],"parent":{"content_type":null,"file_type":null,
            "id":null,"name":null,"parent_id":null,"size":null},
            "status":"OK","total":0}"#;
        let files: Files = serde_json::from_str(json).unwrap();
        assert!(files.files.is_empty());
        assert_eq!(files.status, "OK");
    }

    #[test]
    fn config_invalid_parent_id() {
        let result = Config::try_parse_from(["putio", "not_a_number"]);
        assert!(result.is_err());
    }

    #[test]
    fn config_invalid_max_display() {
        let result = Config::try_parse_from(["putio", "123", "--max-display", "not_a_number"]);
        assert!(result.is_err());
    }
}

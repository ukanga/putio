use clap::{Parser, ValueEnum};
use log::debug;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;

#[derive(Debug)]
struct PutioError(String);

impl fmt::Display for PutioError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "putio error: {}", self.0)
    }
}
impl Error for PutioError {}

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
/// `parent_id` is required and positional. The OAuth token is read from the
/// `PUTIO_TOKEN` environment variable in [`Config::resolve_token`].
#[derive(Parser, Debug)]
#[command(about = "List, download, and clean up files on put.io")]
pub struct Config {
    /// Parent folder ID on put.io
    pub parent_id: i64,

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

    /// OAuth token, populated from the `PUTIO_TOKEN` environment variable.
    #[arg(skip)]
    pub oauth_token: String,
}

impl Config {
    /// Resolve the OAuth token from the `PUTIO_TOKEN` environment variable.
    ///
    /// # Errors
    ///
    /// Returns an error if `PUTIO_TOKEN` is unset or empty.
    pub fn resolve_token(&mut self) -> Result<(), &'static str> {
        match std::env::var("PUTIO_TOKEN") {
            Ok(token) if !token.is_empty() => {
                self.oauth_token = token;
                Ok(())
            }
            _ => Err("PUTIO_TOKEN environment variable is required"),
        }
    }
}

struct Client {
    oauth_token: String,
}

impl Client {
    pub fn new(oauth_token: &str) -> Client {
        let oauth_token = oauth_token.to_owned();

        Client { oauth_token }
    }

    pub fn get(&self, url: &str) -> Result<reqwest::blocking::RequestBuilder, Box<dyn Error>> {
        let client = reqwest::blocking::Client::new();
        let headers = get_authorization_header(&self.oauth_token)?;

        Ok(client.get(url).headers(headers))
    }

    pub fn post(&self, url: &str) -> Result<reqwest::blocking::RequestBuilder, Box<dyn Error>> {
        let client = reqwest::blocking::Client::new();
        let headers = get_authorization_header(&self.oauth_token)?;

        Ok(client.post(url).headers(headers))
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
    parent: File,
    status: String,
    total: i32,
}

fn get_access_token_header(access_token: &str) -> String {
    format!("Token {}", access_token)
}

fn get_authorization_header(
    oauth_token: &str,
) -> Result<reqwest::header::HeaderMap, Box<dyn Error>> {
    let token = get_access_token_header(oauth_token);
    let mut headers = reqwest::header::HeaderMap::with_capacity(1);
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&token)?,
    );

    Ok(headers)
}

fn list_dir(dir: &Path) -> io::Result<Vec<String>> {
    let mut all_files: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
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

fn delete_file(client: &Client, file_id: i64) -> Result<(), Box<dyn Error>> {
    let url = "https://api.put.io/v2/files/delete".to_string();
    let body = json!({ "file_ids": file_id }).to_string();
    let _response = client
        .post(&url)?
        .header(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        )
        .body(body)
        .send()?;

    Ok(())
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
) -> Result<Vec<File>, Box<dyn Error>> {
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
    let response = client.get(&url)?.send()?;
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
        Err(Box::new(PutioError(format!(
            "Error listing files for parent_id={}: {}",
            parent_id,
            api_error_message(response, status)
        ))))
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
) -> Result<Vec<File>, Box<dyn Error>> {
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

fn file_url(client: &Client, file_id: i64) -> Result<String, Box<dyn Error>> {
    let url = format!("https://api.put.io/v2/files/{}/url", file_id);
    debug!("Fetching download URL for file_id={}", file_id);
    let response = client.get(&url)?.send()?;
    if response.status().is_success() {
        let file_url: FileURL = response.json()?;
        debug!("Got download URL for file_id={}", file_id);
        Ok(file_url.url)
    } else {
        let status = response.status();
        debug!("file_url failed for file_id={}: HTTP {}", file_id, status);
        Err(Box::new(PutioError(format!(
            "Error fetching download url for file_id={}: {}",
            file_id,
            api_error_message(response, status)
        ))))
    }
}

fn is_unwanted_file(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").trim().to_lowercase();
    let unwanted = ["exe", "nfo", "txt", "png"];
    unwanted.contains(&ext.as_str())
}

pub fn run(config: Config) -> Result<(), Box<dyn Error>> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let path = Path::new(".");
    let downloaded_files = list_dir(path)?;
    let client = Client::new(&config.oauth_token);

    loop {
        let files = files_list(
            &client,
            config.parent_id,
            config.per_page,
            config.file_type.map(FileType::as_str),
        )?;
        debug!("Total files to process: {}", files.len());
        let mut num_ouputted: u32 = 0;
        let mut unwanted_deleted: u32 = 0;

        for file in files {
            debug!(
                "Processing file: {} (type: {}, id: {})",
                file.name, file.file_type, file.id
            );
            if file.file_type == "VIDEO" {
                let download_url = file_url(&client, file.id)?;
                if !downloaded_files.iter().any(|x| x == &file.name) {
                    if config.max_display.is_none_or(|max| num_ouputted < max) {
                        num_ouputted += 1;
                        writeln!(handle, "{}", download_url)?;
                    }
                } else {
                    // Delete the file first
                    debug!("Deleting file with ID: {} (file: {})", file.id, file.name);
                    delete_file(&client, file.id)?;
                    // Check if the parent can now be deleted
                    if file.parent_id != config.parent_id
                        && parent_safe_to_delete(&client, &file, config.per_page)?
                    {
                        debug!(
                            "Deleting parent directory with ID: {} (file: {})",
                            file.parent_id, file.name
                        );
                        delete_file(&client, file.parent_id)?;
                    }
                    let completed_dir = Path::new("completed");
                    if !completed_dir.is_dir() {
                        fs::create_dir(completed_dir)?;
                    }
                    let completed_path = completed_dir.join(&file.name);
                    fs::rename(&file.name, completed_path)?;
                }
            } else if file.file_type == "ARCHIVE"
                || file.file_type == "IMAGE"
                || is_unwanted_file(&file.name)
            {
                debug!("Deleting file with ID: {} (file: {})", file.id, file.name);
                delete_file(&client, file.id)?;
                unwanted_deleted += 1;
                if file.parent_id != config.parent_id
                    && parent_safe_to_delete(&client, &file, config.per_page)?
                {
                    debug!(
                        "Deleting empty parent directory with ID: {} (file: {})",
                        file.parent_id, file.name
                    );
                    delete_file(&client, file.parent_id)?;
                }
            } else {
                debug!(
                    "Found this other file {} of type {}",
                    file.name, file.file_type
                );
            }
        }

        if num_ouputted > 0 || unwanted_deleted == 0 {
            break;
        }
        debug!(
            "Deleted {} unwanted files, retrying files_list",
            unwanted_deleted
        );
    }

    Ok(())
}

fn parent_safe_to_delete(
    client: &Client,
    file: &File,
    per_page: i8,
) -> Result<bool, Box<dyn Error>> {
    debug!(
        "Checking if parent {} is safe to delete (file: {})",
        file.parent_id, file.name
    );
    // Always ask for VIDEO, whatever the caller filtered on: the rule is
    // "keep folders that still hold videos", and a FILE or IMAGE filter
    // would hide every video sibling from this check.
    let files = files_list(client, file.parent_id, per_page, Some("VIDEO"))?;
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

    // Serialize tests that mutate the PUTIO_TOKEN env var so cargo's parallel
    // test runner can't race set/remove against another test's read.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn config_missing_parent_id() {
        let result = Config::try_parse_from(["putio"]);
        assert!(result.is_err(), "Should fail when parent_id is missing");
    }

    #[test]
    fn config_resolve_token_missing_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("PUTIO_TOKEN");
        let mut config = Config::try_parse_from(["putio", "921234"]).expect("parse");
        let result = config.resolve_token();
        assert!(result.is_err());
    }

    #[test]
    fn config_resolve_token_from_env() -> Result<(), String> {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("PUTIO_TOKEN", "test_token_from_env");
        let mut config = Config::try_parse_from(["putio", "921234"]).map_err(|e| e.to_string())?;
        let resolved = config.resolve_token();
        std::env::remove_var("PUTIO_TOKEN");
        resolved.map_err(|e| e.to_string())?;
        assert_eq!(config.oauth_token, "test_token_from_env");
        assert_eq!(config.parent_id, 921234);
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

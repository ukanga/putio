extern crate reqwest;
extern crate serde;
extern crate serde_json;

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

// #[derive(Debug)]
pub struct Config {
    oauth_token: String,
    parent_id: i64,
    max_display: Option<u32>,
    per_page: i8,
}

impl Config {
    pub fn new(args: &[String]) -> Result<Config, &'static str> {
        let (oauth_token, arg_offset) = match std::env::var("PUTIO_TOKEN") {
            Ok(token) if !token.is_empty() => (token, 0),
            _ => {
                if args.len() < 3 {
                    return Err("Not enough arguments: set PUTIO_TOKEN env var or pass oauth_token as first argument, followed by parent_id.");
                }
                (args[1].clone(), 1)
            }
        };

        if args.len() < 2 + arg_offset {
            return Err("Not enough arguments: parent_id is required.");
        }

        let parent_id: i64 = args[1 + arg_offset].parse::<i64>().unwrap();
        let mut max_display: Option<u32> = None;
        let mut per_page = 1;
        if args.len() >= 3 + arg_offset {
            max_display = Some(args[2 + arg_offset].parse::<u32>().unwrap());
        }
        if args.len() >= 4 + arg_offset {
            per_page = args[3 + arg_offset].parse::<i8>().unwrap();
        }

        Ok(Config {
            oauth_token,
            parent_id,
            max_display,
            per_page,
        })
    }
}

struct Client {
    oauth_token: String,
}

impl Client {
    pub fn new(oauth_token: &String) -> Client {
        let oauth_token = oauth_token.clone();

        Client { oauth_token }
    }

    pub fn get(&self, url: &String) -> Result<reqwest::blocking::RequestBuilder, Box<dyn Error>> {
        let client = reqwest::blocking::Client::new();
        let headers = get_authorization_header(&self.oauth_token);

        Ok(client.get(url).headers(headers))
    }

    pub fn post(&self, url: &String) -> Result<reqwest::blocking::RequestBuilder, Box<dyn Error>> {
        let client = reqwest::blocking::Client::new();
        let headers = get_authorization_header(&self.oauth_token);

        Ok(client.post(url).headers(headers))
    }
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

fn get_access_token_header(access_token: &String) -> String {
    format!("Token {}", access_token)
}

fn get_authorization_header(oauth_token: &String) -> reqwest::header::HeaderMap {
    let token = get_access_token_header(oauth_token);
    let mut headers = reqwest::header::HeaderMap::with_capacity(1);
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&token).unwrap(),
    );

    headers
}

fn list_dir(dir: &Path) -> io::Result<Vec<String>> {
    let mut all_files: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let path = path.file_name().unwrap().to_str().unwrap();
            all_files.push(String::from(path));
        }
    }
    for file in &all_files {
        let aria2_extension = ".aria2";
        if !file.ends_with(&aria2_extension) {
            let aria2_file = format!("{}.aria2", file);
            if file != &aria2_file {
                if all_files.iter().find(|&x| x == &aria2_file) == None {
                    files.push(String::from(file.clone()));
                }
            }
        }
    }
    Ok(files)
}

fn delete_file(client: &Client, file_id: i64) -> Result<(), Box<dyn Error>> {
    let url = format!("https://api.put.io/v2/files/delete");
    let body = json!({ "file_ids": file_id }).to_string();
    let _response = client
        .post(&url)
        .unwrap()
        .header(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        )
        .body(body)
        .send()?;

    Ok(())
}

fn files_list(client: &Client, parent_id: i64, per_page: i8) -> Result<Vec<File>, Box<dyn Error>> {
    let url = format!(
        "https://api.put.io/v2/files/list?parent_id={}&per_page={}",
        parent_id, per_page
    );
    debug!(
        "Listing files for parent_id={} per_page={}",
        parent_id, per_page
    );
    let response = client.get(&url).unwrap().send()?;
    let mut files: Vec<File> = Vec::new();
    if response.status().is_success() {
        let response_files: Files = response.json()?;
        debug!(
            "Got {} files (status: {})",
            response_files.files.len(),
            response_files.status
        );
        for file in response_files.files {
            if file.file_type == "FOLDER" {
                let folder = files_list(&client, file.id, per_page).unwrap();
                for f in folder {
                    files.push(f);
                }
            } else {
                files.push(file);
            }
        }
    } else {
        debug!(
            "files_list failed for parent_id={}: HTTP {}",
            parent_id,
            response.status()
        );
    }

    Ok(files)
}

fn file_url(client: &Client, file_id: i64) -> Result<String, Box<dyn Error>> {
    let url = format!("https://api.put.io/v2/files/{}/url", file_id);
    debug!("Fetching download URL for file_id={}", file_id);
    let response = client.get(&url).unwrap().send()?;
    if response.status().is_success() {
        let file_url: FileURL = response.json()?;
        debug!("Got download URL for file_id={}", file_id);
        Ok(file_url.url)
    } else {
        debug!(
            "file_url failed for file_id={}: HTTP {}",
            file_id,
            response.status()
        );
        return Err(Box::new(PutioError(
            "Error fetching file download url.".into(),
        )));
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
    let downloaded_files = list_dir(&path).unwrap();
    let client = Client::new(&config.oauth_token);

    loop {
        let files = files_list(&client, config.parent_id, config.per_page).unwrap();
        debug!("Total files to process: {}", files.len());
        let mut num_ouputted: u32 = 0;
        let mut unwanted_deleted: u32 = 0;

        for file in files {
            debug!(
                "Processing file: {} (type: {}, id: {})",
                file.name, file.file_type, file.id
            );
            if file.file_type == "VIDEO" {
                let download_url = file_url(&client, file.id).unwrap();
                if downloaded_files.iter().find(|&x| x == &file.name) == None {
                    if config.max_display.is_none() || num_ouputted < config.max_display.unwrap() {
                        num_ouputted += 1;
                        writeln!(handle, "{}", download_url)?;
                    }
                } else {
                    // Delete the file first
                    debug!("Deleting file with ID: {} (file: {})", file.id, file.name);
                    delete_file(&client, file.id).unwrap();
                    // Check if the parent can now be deleted
                    if file.parent_id != config.parent_id
                        && parent_safe_to_delete(&client, &file, config.per_page)
                    {
                        debug!(
                            "Deleting parent directory with ID: {} (file: {})",
                            file.parent_id, file.name
                        );
                        delete_file(&client, file.parent_id).unwrap();
                    }
                    let completed_dir = Path::new("completed");
                    if !completed_dir.is_dir() {
                        fs::create_dir(completed_dir).unwrap();
                    }
                    let completed_path = completed_dir.join(String::from(format!("{}", file.name)));
                    fs::rename(file.name, completed_path).unwrap();
                }
            } else if file.file_type == "ARCHIVE" || is_unwanted_file(&file.name) {
                debug!("Deleting file with ID: {} (file: {})", file.id, file.name);
                delete_file(&client, file.id).unwrap();
                unwanted_deleted += 1;
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

fn parent_safe_to_delete(client: &Client, file: &File, per_page: i8) -> bool {
    debug!(
        "Checking if parent {} is safe to delete (file: {})",
        file.parent_id, file.name
    );
    let files = files_list(&client, file.parent_id, per_page).unwrap();
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
            return false;
        }
    }
    debug!("No other videos in parent, parent IS safe to delete");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_not_enough_args_no_env() -> Result<(), String> {
        std::env::remove_var("PUTIO_TOKEN");
        let args: Vec<String> = vec![String::from(""), String::from("")];
        let config = Config::new(&args);
        let result = match config {
            Ok(_config) => Err(String::from("Should fail with missing args")),
            Err(err) => {
                assert!(
                    err.contains("Not enough arguments"),
                    "Not enough arguments error message not present in error."
                );
                Ok(())
            }
        };

        result
    }

    #[test]
    fn config_has_enough_args_no_env() -> Result<(), String> {
        std::env::remove_var("PUTIO_TOKEN");
        let args: Vec<String> = vec![
            String::from("putio"),
            String::from("api_token"),
            String::from("921234"),
        ];
        let config = Config::new(&args);
        let result = match config {
            Ok(_config) => Ok(()),
            Err(err) => Err(String::from(format!("Unexpected error raised: {}", err))),
        };

        result
    }

    #[test]
    fn config_with_env_token() -> Result<(), String> {
        std::env::set_var("PUTIO_TOKEN", "test_token_from_env");
        let args: Vec<String> = vec![String::from("putio"), String::from("921234")];
        let config = Config::new(&args);
        std::env::remove_var("PUTIO_TOKEN");
        match config {
            Ok(config) => {
                assert_eq!(config.oauth_token, "test_token_from_env");
                assert_eq!(config.parent_id, 921234);
                Ok(())
            }
            Err(err) => Err(String::from(format!("Unexpected error raised: {}", err))),
        }
    }
}

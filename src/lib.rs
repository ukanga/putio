extern crate reqwest;
extern crate serde;
extern crate serde_json;

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::error::Error;
use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;

// #[derive(Debug)]
pub struct Config {
    oauth_token: String,
    parent_id: i64,
    max_display: Option<u32>,
}

impl Config {
    pub fn new(args: &[String]) -> Result<Config, &'static str> {
        if args.len() < 3 {
            return Err("Not enough arguments: oauth_token and parent_id folder are required.");
        }

        let oauth_token = args[1].clone();
        let parent_id: i64 = args[2].parse::<i64>().unwrap();
        let mut max_display: Option<u32> = None;
        if args.len() == 4 {
            max_display = Some(args[3].parse::<u32>().unwrap());
        }

        Ok(Config {
            oauth_token,
            parent_id,
            max_display,
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

fn files_list(client: &Client, parent_id: i64) -> Result<Vec<File>, Box<dyn Error>> {
    let url = format!("https://api.put.io/v2/files/list?parent_id={}", parent_id);
    let response = client.get(&url).unwrap().send()?;
    let mut files: Vec<File> = Vec::new();
    if response.status().is_success() {
        let response_files: Files = response.json()?;
        for file in response_files.files {
            if file.file_type == "FOLDER" {
                let folder = files_list(&client, file.id).unwrap();
                for f in folder {
                    files.push(f);
                }
            } else {
                files.push(file);
            }
        }
    }

    Ok(files)
}

fn file_url(client: &Client, file_id: i64) -> Result<String, Box<dyn Error>> {
    let url = format!("https://api.put.io/v2/files/{}/url", file_id);
    let response: FileURL = client.get(&url).unwrap().send()?.json()?;

    Ok(response.url)
}

pub fn run(config: Config) -> Result<(), Box<dyn Error>> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let path = Path::new(".");
    let downloaded_files = list_dir(&path).unwrap();
    let client = Client::new(&config.oauth_token);
    let files = files_list(&client, config.parent_id).unwrap();
    let mut num_ouputted: u32 = 0;
    for file in files {
        if file.file_type == "VIDEO" {
            let download_url = file_url(&client, file.id).unwrap();
            if downloaded_files.iter().find(|&x| x == &file.name) == None {
                if config.max_display.is_none() || num_ouputted < config.max_display.unwrap() {
                    num_ouputted += 1;
                    writeln!(handle, "{}", download_url)?;
                }
            } else {
                if file.parent_id != config.parent_id && parent_safe_to_delete(&client, &file) {
                    delete_file(&client, file.parent_id).unwrap();
                } else {
                    delete_file(&client, file.id).unwrap();
                }
                let completed_dir = Path::new("completed");
                if !completed_dir.is_dir() {
                    fs::create_dir(completed_dir).unwrap();
                }
                let completed_path = completed_dir.join(String::from(format!("{}", file.name)));
                fs::rename(file.name, completed_path).unwrap();
            }
        }
    }

    Ok(())
}

fn parent_safe_to_delete(client: &Client, file: &File) -> bool {
    let files = files_list(&client, file.parent_id).unwrap();
    for file_in_dir in files {
        if file_in_dir.file_type == "VIDEO" && file_in_dir.id != file.id {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_not_enough_args() -> Result<(), String> {
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
    fn config_has_enough_args() -> Result<(), String> {
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
}

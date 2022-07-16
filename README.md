# Putio CLI - list file URLs.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)

Implements a library for put.io API access as well as a cli tool that lists
video files. If a video is already downloaded it moves the file to the
`completed` folder in the current directory and then deletes the file from
put.io - inorder to create more room on allocated space on put.io.

Start by registering your application and obtaining your API credentials from
[OAuth Apps page](https://app.put.io/settings/account/oauth/apps).

## Usage

**Generate a list of file download URLs in a given folder:**

```sh
putio <put.io OAuth token> <folder-id from app.put.io/files/folder-id>
```

  ***Note***: If a file in the current folder has the same name as a file on the
  specified put.io folder will be deleted.


**Download the file at the file download URLS using `aria2c` to the current folder.**

```sh
putio <put.io OAuth token> <folder-id from app.put.io/files/folder-id> \
    | xargs aria2c -x 5 --auto-file-renaming=false --allow-overwrite=false
```

**Download the file at the file download URLS using `aria2c`** to the current folder
and **delete** the completed downloaded files form put.io. This is simply by
running the same `putio` command in a pipe command as shown below.

```sh
putio <put.io OAuth token> <folder-id from app.put.io/files/folder-id> \
    | xargs aria2c -x 5 --auto-file-renaming=false --allow-overwrite=false \
    && putio <put.io OAuth token> <folder-id from app.put.io/files/folder-id>
```

**You can also combine this with the [Filesystem Librarian
tool](https://github.com/jasonrogena/librarian)** to move the downloaded files to
your preferred location. For example with the configuration below for
`fs-librarian` you can move the downloaded file to a different folder.

```toml
[libraries.tvshows]
command = """
mkdir -p ./Done && mv "{{ file_path }}" ./Done/
"""

  [libraries.tvshows.filter]
  directories = [ "./completed" ]
  mime_type_regexs = [ "application/x-matroska" ]
```

Using the above `librarian.toml` configuration for `fs-librarian` you will use
the commands:

```sh
putio <put.io OAuth token> <folder-id from app.put.io/files/folder-id> \
    | xargs aria2c -x 5 --auto-file-renaming=false --allow-overwrite=false \
    && putio <put.io OAuth token> <folder-id from app.put.io/files/folder-id> \
    && fs-librarian single-shot librarian.toml
```

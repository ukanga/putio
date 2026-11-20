# Putio CLI - list file URLs.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)

Implements a library for put.io API access as well as a CLI tool that lists
file download URLs. When a file in the current directory has the same name as
one on put.io, the local file is moved to `completed/` and the put.io copy is
deleted to free up allocated space.

Start by registering your application and obtaining your API credentials from
[OAuth Apps page](https://app.put.io/settings/account/oauth/apps). Export the
token as `PUTIO_TOKEN` so it isn't passed on the command line:

```sh
export PUTIO_TOKEN=<your token>
```

## Usage

**List download URLs for every file in a folder:**

```sh
putio <folder-id from app.put.io/files/folder-id>
```

**Filter to a specific put.io file type** (e.g. only videos):

```sh
putio <folder-id> --file-type VIDEO
```

The accepted types are `VIDEO`, `AUDIO`, `IMAGE`, `ARCHIVE`, `PDF`, `TEXT` and
`FILE`; anything else is rejected before any request is made. `FOLDER` is not
one of them, because deleting a folder on put.io deletes everything inside it.
`--file-type` and `--delete-type` are mutually exclusive.

**Limit how many URLs are printed** (e.g. one URL at a time):

```sh
putio <folder-id> --file-type VIDEO --max-display 1
```

`--per-page` sets how many entries each put.io listing request asks for
(default 1). Raising it looks at more files per run; `--max-display` still caps
how many URLs are printed.

**Pipe the URLs to `aria2c` to download them:**

```sh
putio <folder-id> --file-type VIDEO \
    | xargs aria2c -x 5 --auto-file-renaming=false --allow-overwrite=false
```

**Download then clean up** — running the same command after `aria2c` finishes
detects the local files, deletes them from put.io, and moves them to `completed/`:

```sh
putio <folder-id> --file-type VIDEO \
    | xargs aria2c -x 5 --auto-file-renaming=false --allow-overwrite=false \
    && putio <folder-id> --file-type VIDEO
```

**Cleanup mode** — delete unwanted file types from put.io without downloading
anything (repeatable):

```sh
putio <folder-id> --delete-type ARCHIVE --delete-type IMAGE
```

**You can also combine this with the [Filesystem Librarian
tool](https://github.com/jasonrogena/librarian)** to move the downloaded files
to your preferred location:

```toml
[libraries.tvshows]
command = """
mkdir -p ./Done && mv "{{ file_path }}" ./Done/
"""

  [libraries.tvshows.filter]
  directories = [ "./completed" ]
  mime_type_regexs = [ "application/x-matroska" ]
```

Then chain everything together:

```sh
putio <folder-id> --file-type VIDEO \
    | xargs aria2c -x 5 --auto-file-renaming=false --allow-overwrite=false \
    && putio <folder-id> --file-type VIDEO \
    && fs-librarian single-shot librarian.toml
```

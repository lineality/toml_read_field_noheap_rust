# read_toml_single_line_string_field_no_heap

A Rust module that reads a single top-level key-value field from a TOML
file using only stack memory. No heap allocation occurs during the read.

## Purpose

Production deployments that load, often few and selective,
short configuration values (identifiers, mode flags, port numbers, paths) 
from a TOML file at startup may need to avoid (or benefit from avoiding)
heap allocation. 

Standard-library idioms such as `BufReader::lines()`
allocate on the heap for internal buffering and for each returned `String`.

This module provides a focused alternative: it scans a TOML file one byte
per read call for a single named key and writes only that key's value into
a fixed-size stack buffer chosen by the caller.

The 'type' that the raw output becomes is entirely flexible and up to
the user-caller, however they need to handle it.

This is a small very-modular module that should be easy to incorperate
into other projects, either added to a simple flat-file project or added
to a set of separate module files.

The error and error-code system follows this production-oriented system:
https://github.com/lineality/modes_and_case_handling


## What it does

- Reads the value of one top-level `key = value` line per call.
- Returns the value as raw bytes in a caller-sized `[u8; N]` buffer, plus a length.
- Supports quoted (`"value"`) and unquoted (`8080`) values.
- Handles LF and CRLF line endings.
- Skips blank lines and lines whose first non-whitespace byte is `#`.
- Refuses values longer than the buffer (never truncates).
- Optionally checks that given keys are assigned on at most one line.
- Returns a 2-byte fieldless error code on failure: no heap, no path, no
  file contents, no OS error text.
- Never panics in release builds; all loops are bounded.

## What it does not do

- Parse full TOML: no sections, arrays, tables, inline tables, multi-line
  strings, escape sequences (a `\` is copied as a plain byte), dotted keys,
  or datetimes.
- Support quoted keys (`"a b" = 1`), or keys containing spaces, tabs, or `#`.
  Only bare keys, matched exactly (case-sensitive).
- Convert the value to a type: the caller decides (see "Typed values").
- Remove an inline comment after an unquoted value (see "Limitations").
- Strip a UTF-8 BOM at the start of the file.

## Value rules

- Quoted values end at the next `"`. End of file before it: code 813.
- Unquoted values end at the next newline. A `\r` directly before the
  newline is not part of the value. End of file before the newline: code 813.
- Spaces or tabs before the newline of an unquoted value ARE part of the
  value; trim if needed.
- `key =` followed by a newline, and `key = ""`, are valid empty values.
- A longer key that starts with the requested key (`name_long` vs `name`)
  does not match.

## API

```rust
pub fn read_single_line_string_field_from_toml_no_heap<const OUTPUT_BUFFER_BYTES: usize>(
    absolute_toml_file_path: &Path,
    target_field_key: &str,
) -> Result<([u8; OUTPUT_BUFFER_BYTES], usize), ReadTomlFieldError>

pub fn count_toml_key_lines_no_heap(
    absolute_toml_file_path: &Path,
    target_field_key: &str,
) -> Result<u32, ReadTomlFieldError>

pub fn check_toml_keys_are_unique_no_heap(
    absolute_toml_file_path: &Path,
    target_field_keys: &[&str],
) -> Result<(), ReadTomlFieldError>
```

- The path must be absolute (code 803). The key must not be empty (code 801).
- `OUTPUT_BUFFER_BYTES` is a const generic chosen at each call site. It is
  the longest value that call accepts; longer values return code 812.
  Different call sites can use different sizes.
- On success, `buffer[..length]` is the value; the remaining bytes are 0.
- If a key appears more than once, the read returns the first assignment.
- `count_toml_key_lines_no_heap` counts the lines that assign a key.
- `check_toml_keys_are_unique_no_heap` returns code 816 if any listed key
  is assigned on more than one line. Keys not in the list are not checked.

### Optional uniqueness check

A hand-edited file with a key written twice would otherwise silently use
the first value. To make that an error, run the check before reading:

```rust
check_toml_keys_are_unique_no_heap(config_path, &["node_id", "mode", "port"])?;
```

The error code does not say which key; call `count_toml_key_lines_no_heap`
per key if you need to know.

## Typed values

The reader returns raw bytes. The caller chooses the type by converting
after the read:

| Wanted type | Conversion                                                  |
|-------------|-------------------------------------------------------------|
| text        | `core::str::from_utf8(bytes)`                               |
| integer     | text, then `.parse::<u16>()` (or another integer type)      |
| boolean     | text, then `.parse::<bool>()` (exactly `true` / `false`)    |
| path        | text, then `Path::new(text)`; check `.is_absolute()` if needed |

Choose the buffer size per field, e.g. 8 bytes for a `u16`, 4096 bytes
for a Linux path. `main.rs` (`demonstrate_typed_values`) shows an integer
and a path.

## Error codes

`ReadTomlFieldError` is a fieldless `#[repr(u16)]` enum: each value is
2 bytes, `Copy`, and carries no data. `.code()` returns the number, which
is safe to log in any build. `Debug` and `Display` (descriptive text)
exist only in debug and test builds.

| Code | Variant                           | Meaning                                              |
|------|-----------------------------------|------------------------------------------------------|
| 801  | RsLsfEmptyKey                     | key is empty (caller bug)                            |
| 802  | RsLsfOutputBufferZeroSized        | `OUTPUT_BUFFER_BYTES` is 0 (caller bug)              |
| 803  | RsLsfPathNotAbsolute              | path is not absolute                                 |
| 804  | RsLsfOpenNotFound                 | file not found                                       |
| 805  | RsLsfOpenPermissionDenied         | permission denied                                    |
| 806  | RsLsfOpenTransient                | transient open error, retries exhausted (retryable)  |
| 807  | RsLsfOpenOther                    | any other open failure                               |
| 808  | RsLsfReadFailed                   | a read failed                                        |
| 809  | RsLsfReadCallLimitReached         | reads kept being interrupted (bounded allowance used up) |
| 810  | RsLsfFileExceedsScanLimit         | file larger than `RSLSF_MAX_BYTES_SCANNED`           |
| 811  | RsLsfFieldNotFound                | no line assigns the key                              |
| 812  | RsLsfValueExceedsOutputBuffer     | value longer than the buffer                         |
| 813  | RsLsfValueUnterminatedAtEndOfFile | end of file inside a value                           |
| 814  | RsLsfKeyMatchIndexInvalid         | internal invariant (should not happen)               |
| 815  | RsLsfScanOutcomeMismatch          | internal invariant (should not happen)               |
| 816  | RsLsfDuplicateKey                 | a checked key is assigned on more than one line      |

Codes are append-only: never renumbered or reused. The numbering matches
the project this module was written for (block 800-849; 817-849 reserved).
`.is_retryable()` is true only for 806.

### Behavior by build mode

- Release: never panics; returns the code only; prints nothing.
- Debug: prints a diagnostic line for each error (may include the path and
  key). Codes 801, 802, 814, and 815 are caller bugs or internal invariants
  and also panic in debug builds, to make the bug visible.
- Test: unit tests assert the returned codes.

## Usage

```rust
use std::path::Path;
use read_toml_single_line_string_field_no_heap::{
    read_single_line_string_field_from_toml_no_heap,
    ReadTomlFieldError,
};

let config_path = Path::new("/etc/myapp/config.toml");

match read_single_line_string_field_from_toml_no_heap::<16>(config_path, "node_id") {
    Ok((buffer, length)) => {
        match buffer.get(..length).and_then(|bytes| core::str::from_utf8(bytes).ok()) {
            Some(node_id) => { /* use node_id */ }
            None => { /* value is not UTF-8: use a default, or report */ }
        }
    }
    Err(ReadTomlFieldError::RsLsfFieldNotFound) => {
        // key absent: use a default
    }
    Err(error_code) => {
        // report error_code.code(); do not expose the path or file contents
    }
}
```

## Memory and limits

| Item                 | Size                                  | Set by                   |
|----------------------|---------------------------------------|--------------------------|
| Output value buffer  | `OUTPUT_BUFFER_BYTES`                 | caller (const generic)   |
| Read scratch         | 1 byte (`[u8; 1]`)                    | module                   |
| Scan state           | a few machine words                   | module                   |

There is no read-chunk buffer and no line buffer: lines are never
assembled, so line length does not matter. Only the matched value must
fit the output buffer.

Limits (module constants; they bound work, not memory):

| Constant                   | Value | Effect                                         |
|----------------------------|------:|------------------------------------------------|
| `RSLSF_MAX_BYTES_SCANNED`  | 1 MiB | larger file: code 810                          |
| `RSLSF_EXTRA_READ_CALLS`   |  1024 | allowance for interrupted reads: code 809      |
| `RSLSF_MAX_OPEN_ATTEMPTS`  |     3 | transient open errors retried with backoff     |

## Dependencies

None. Standard library only (`std::fs::File`, `std::io::Read`,
`std::path::Path`, `std::time::Duration`, `std::thread::sleep`).

## Running tests

```
cargo test
cargo test --release
```

Scanner tests run on in-memory input. File tests create uniquely named
files under the OS temp directory and remove them.

## Limitations

- One key per call; each call opens the file and scans it again. Reading N
  keys means N passes. For small configuration files this is negligible.
- One read system call per byte. Fine for small files read a few times; a
  large file read often would favor a chunked reader (out of scope).
- First match wins when a key appears more than once (see the optional
  uniqueness check).
- No `[section]` awareness: all keys are treated as top-level.
- Inline comments after unquoted values become part of the value:
  `port = 8080  # note` yields `8080  # note`. After quoted values they
  are ignored: `port = "8080"  # note` yields `8080`.
- A quoted value containing a newline is read across the newline;
  key-line counting does not know about such values.

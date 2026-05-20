# read_toml_single_line_string_field_no_heap

A Rust module that reads a single top-level key-value field from a TOML
file using only stack-allocated memory. No heap allocation occurs during
the read.

## Purpose

Production deployments that load short configuration values (identifiers,
mode flags, port numbers) from a TOML file at startup may need to avoid
heap allocation. Standard-library idioms such as `BufReader::lines()`
allocate on the heap for internal buffering and for each returned `String`.

This module provides a focused alternative: one function that scans a
TOML file for a single named key and returns the value in a fixed-size
stack buffer.

## What it does

- Reads one top-level `key = "value"` pair per call.
- Returns the value in a caller-sized `[u8; N]` buffer with a length.
- Supports both quoted (`"value"`) and unquoted (`8080`) values.
- Handles LF and CRLF line endings.
- Skips blank lines and `#`-comment lines.
- Returns a zero-sized error enum on failure (no heap, no data leakage).
- Bounds all loops by a configurable byte budget (default 1 MiB).

## What it does not do

- Parse full TOML (no sections, arrays, tables, inline tables, multi-line
  strings, escape sequences, dotted keys, or datetimes).
- Validate UTF-8 in the value (caller decides whether to check).
- Handle inline comments on the same line as the value.
- Handle keys that contain `=` or `"`.
- Strip a UTF-8 BOM at the start of the file.

## API

```rust
pub fn read_single_line_string_field_from_toml_no_heap<
    const OUTPUT_BUFFER_BYTES: usize,
>(
    absolute_toml_file_path: &str,
    target_field_key: &str,
) -> Result<([u8; OUTPUT_BUFFER_BYTES], usize), ReadTomlFieldError>
```

`OUTPUT_BUFFER_BYTES` is a const generic chosen by the caller at each
call site. It sets the maximum value length that call will accept.
Values longer than `OUTPUT_BUFFER_BYTES` produce
`RsLsfValueExceedsOutputBuffer`, never silent truncation.

Two internal buffer sizes are set by module-level constants:

| Constant                   | Default | Role                          |
|----------------------------|--------:|-------------------------------|
| `RSLSF_READ_CHUNK_BYTES`  |   256 B | File-read chunk (syscall)     |
| `RSLSF_MAX_LINE_BYTES`    |   512 B | Single-line accumulator       |
| `RSLSF_MAX_BYTES_SCANNED` |   1 MiB | Failsafe byte budget per call |

These can be lowered for constrained environments or promoted to const
generics if per-call control is needed.

## Error type

```rust
pub enum ReadTomlFieldError {
    RsLsfEmptyKey,
    RsLsfKeyTooLong,
    RsLsfOutputBufferZeroSized,
    RsLsfFileOpenFailed,
    RsLsfFileReadFailed,
    RsLsfFieldNotFound,
    RsLsfValueExceedsOutputBuffer,
    RsLsfMatchingLineExceedsScanBuffer,
    RsLsfSafetyBudgetExhausted,
}
```

All variants are zero-sized. No variant carries a path, file contents,
OS error string, or any other runtime data. Each variant name begins
with `RsLsf` so it can be traced to this function in logs.

## Usage

```rust
use read_toml_single_line_string_field_no_heap::{
    read_single_line_string_field_from_toml_no_heap,
    ReadTomlFieldError,
};

match read_single_line_string_field_from_toml_no_heap::<16>(
    "/etc/myapp/config.toml",
    "node_id",
) {
    Ok((buffer, length)) => {
        let value: &str = match core::str::from_utf8(&buffer[..length]) {
            Ok(s) => s,
            Err(_) => { /* handle non-UTF-8 */ return; }
        };
        // use value
    }
    Err(ReadTomlFieldError::RsLsfFieldNotFound) => {
        // key absent; use a default or log a terse code
    }
    Err(_other) => {
        // file unreadable, value too long, etc.
        // log a terse code; do not expose path or contents
    }
}
```

## Stack footprint

For a 16-byte value buffer with default internal constants:

```
Output buffer:       16 B   (caller-chosen)
Read chunk:         256 B   (module constant)
Line accumulator:   512 B   (module constant)
────────────────────────────
Total:             ~784 B   on the stack, for the duration of the call
```

For tighter environments, lower the constants or promote them to
const generics. For example, with a 32 B read chunk and a 64 B line
accumulator, the total drops to ~112 B.

## Dependencies

None. Standard library only (`std::fs::File`, `std::io::Read`).

## Running tests

```
cargo test
cargo test --release
```

## Limitations

- One key per call. Reading N keys requires N passes over the file.
  For small configuration files (< 1 KiB) this is negligible. For
  files with many keys, consider a batch-read variant or a different
  approach.
- First match wins when duplicate keys exist.
- No TOML section (`[section]`) awareness. All keys are treated as
  top-level.
- Inline comments after unquoted values become part of the value.
  For example, `port = 8080  # note` yields `8080  # note`, not
  `8080`. Quoted values are unaffected: `port = "8080"  # note`
  yields `8080`.

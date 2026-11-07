#![forbid(unsafe_code)]
//! # main.rs: Read Single Line String Field = rslsf: rslsf_demo
//!
//! Demonstrates and smoke-tests the no-heap TOML field reader against real files.
//!
//! # Steps
//! 1. Write a small known TOML file to an absolute path under the OS temp
//!    directory (overwritten on every run; demo setup only).
//! 2. Check that the demo keys appear at most once (optional uniqueness check).
//! 3. Read fields from it, including one missing key (expected: code 811).
//! 4. Read two fields from `test.toml` in the current directory, if present.
//!
//! Output is one fixed-format line per step: `OK ...`, or `ERR <key> code <n>`.
//! Error lines carry the numeric code only (no path, no file contents, no OS
//! error text), matching the module's production policy. `println!` is used
//! here for demonstration; production callers route output through their own
//! reporting.

/*
Flexible and Configurable:
- User decides/handles "types" of output.
- User decides size-max of output.

Intended for a standard 'toml/config file read rarely, often at startup'
not designed for high-volume reading.

# Size Instructions
- The demo length 16 is not hardcoded anywhere in the module.
- The `16` lives only in `main.rs`:

```rust
const DEMO_OUTPUT_BUFFER_BYTES: usize = 16;   // demo's choice, not the module's
```

The configurable length is passed to the function as a const generic at the call site:

```rust
read_single_line_string_field_from_toml_no_heap::<DEMO_OUTPUT_BUFFER_BYTES>(path, "node_id")
//                                                ^^^^^^^^^^^^^^^^^^^^^^^^
//                                                Here is the "knob"
```

You can call with any compile-time size you like (`path` is an absolute `&Path`):

```rust
read_single_line_string_field_from_toml_no_heap::<8>(path, "x")
read_single_line_string_field_from_toml_no_heap::<32>(path, "x")
read_single_line_string_field_from_toml_no_heap::<4096>(path, "x")   // e.g. a Linux path value
```

Each call site picks its own value buffer size. They do not interfere with each other.
A value longer than the chosen size returns code 812; it is never truncated.

# Memory used by one call

| Item                 | Purpose                                   | Size                                  | Where set                  | Caller-tunable? |
|----------------------|-------------------------------------------|---------------------------------------|----------------------------|-----------------|
| Output value buffer  | Holds the extracted value                 | `OUTPUT_BUFFER_BYTES` (e.g. 16)       | const generic at call site | Yes             |
| Read scratch         | Receives one byte per read call           | `[u8; 1]`                             | fixed in the module        | No (no reason)  |
| Scan state           | Line state, key-match position, cursors   | a few machine words                   | fixed in the module        | No              |

There is no read-chunk buffer and no line buffer: the file is read one byte
per read call, and lines are never assembled. So line length does not matter;
only the matched value must fit the output buffer.

Limits (counts, not buffers; they bound work, not memory):
- `RSLSF_MAX_BYTES_SCANNED` (1 MiB): a larger file stops with code 810.
- `RSLSF_EXTRA_READ_CALLS` (1024): allowance for "interrupted" reads (code 809).

Tradeoff of the one-byte design: one read syscall per byte. For a small
config file read a few times at startup this is negligible. A large file read
often would favor a chunked reader; that is outside this module's scope.


Expected main-run demo test output:

OK  keys appear at most once: node_id, mode, port
OK  node_id = alice-node-01
OK  mode = production
OK  port = 8080
ERR missing_key code 811
OK  port as u16 = 8080
OK  data_directory as Path = /tmp/rslsf_demo_data
OK  text = production
ERR longtext code 812
*/

mod read_toml_single_line_string_field_no_heap;

use read_toml_single_line_string_field_no_heap::{
    ReadTomlFieldError, check_toml_keys_are_unique_no_heap,
    read_single_line_string_field_from_toml_no_heap,
};
use std::io::Write;
use std::path::Path;

/// Output buffer size for this demo; fits the longest demo value
/// (`alice-node-01`, 13 bytes). Each caller chooses its own size.
const DEMO_OUTPUT_BUFFER_BYTES: usize = 16;

/// Demo TOML file name, written under the OS temp directory.
const DEMO_TOML_FILE_NAME: &str = "rslsf_demo_config.toml";

/// Demo TOML contents, within the module's supported subset.
/// `data_directory` exists to demonstrate reading a value as a path.
const DEMO_TOML_FILE_CONTENTS: &str = "\
# rslsf_demo: sample configuration
# This file is overwritten on every run.

node_id        = \"alice-node-01\"
mode           = \"production\"
port           = 8080
data_directory = \"/tmp/rslsf_demo_data\"
";

/// Keys checked by the optional uniqueness check.
const DEMO_UNIQUE_KEYS: [&str; 3] = ["node_id", "mode", "port"];

/// Optional user test file, read from the current directory.
const USER_TEST_TOML_FILE_NAME: &str = "test.toml";

/// Process exit codes: few and fixed, never raw OS errors.
const EXIT_OK: i32 = 0;
const EXIT_DEMO_SETUP_FAILED: i32 = 10;
const EXIT_READ_FAILED: i32 = 20;

fn main() {
    // Step 1: write the demo file (demo setup, not a production pattern).
    let demo_file_path = std::env::temp_dir().join(DEMO_TOML_FILE_NAME);
    let write_result = std::fs::File::create(&demo_file_path)
        .and_then(|mut demo_file| demo_file.write_all(DEMO_TOML_FILE_CONTENTS.as_bytes()));
    if write_result.is_err() {
        // Terse: no path, no OS error.
        eprintln!("rslsf_demo: setup failed");
        std::process::exit(EXIT_DEMO_SETUP_FAILED);
    }

    // Step 2: optional uniqueness check.
    match check_toml_keys_are_unique_no_heap(&demo_file_path, &DEMO_UNIQUE_KEYS) {
        Ok(()) => println!("OK  keys appear at most once: node_id, mode, port"),
        Err(error_code) => {
            println!("ERR uniqueness check code {}", error_code.code());
            std::process::exit(EXIT_READ_FAILED);
        }
    }

    // Step 3a: independent reads (no shared state between calls).
    print_field_or_terse_error("node_id", &demo_file_path);
    print_field_or_terse_error("mode", &demo_file_path);
    print_field_or_terse_error("port", &demo_file_path);
    print_field_or_terse_error("missing_key", &demo_file_path);

    // Step 3b: the same reader, with the bytes converted to typed values by the caller.
    demonstrate_typed_values(&demo_file_path);

    // Step 4: user test file in the current directory (made absolute).
    let user_test_file_path = match std::env::current_dir() {
        Ok(current_directory) => current_directory.join(USER_TEST_TOML_FILE_NAME),
        Err(_) => {
            eprintln!("rslsf_demo: setup failed");
            std::process::exit(EXIT_DEMO_SETUP_FAILED);
        }
    };
    print_field_or_terse_error("text", &user_test_file_path);
    print_field_or_terse_error("longtext", &user_test_file_path);

    std::process::exit(EXIT_OK);
}

/// Shows how a caller turns the reader's raw bytes into typed values.
///
/// # Project context
/// The reader always returns `([u8; N], written_length)`: raw bytes, never
/// interpreted. Type is the caller's decision, made after reading:
/// - text:    `core::str::from_utf8(bytes)`
/// - integer: text, then `.parse::<u16>()` (or any integer type)
/// - boolean: text, then `.parse::<bool>()` (exactly `true` / `false`)
/// - path:    text, then `Path::new(text)`, then `.is_absolute()` if required
///
/// Each caller also chooses the buffer size per field: 8 bytes is plenty
/// for a `u16` (at most 5 digits); 4096 bytes fits any Linux path (PATH_MAX).
///
/// Conversion failures here are the caller's own cases, not reader codes,
/// so they print a fixed word instead of a number. Note that unquoted values
/// keep trailing spaces (module policy), so `.parse` fails on `8080   `
/// unless the caller trims first; quoted values have no such issue.
///
/// Paths in this example must be valid UTF-8. A Unix-only alternative that
/// accepts any bytes is `std::os::unix::ffi::OsStrExt::from_bytes`.
fn demonstrate_typed_values(absolute_toml_file_path: &Path) {
    // Integer: read into a small buffer, then text, then u16.
    match read_single_line_string_field_from_toml_no_heap::<8>(absolute_toml_file_path, "port") {
        Ok((output_buffer, written_length)) => {
            let parsed_port = output_buffer
                .get(..written_length)
                .and_then(|value_bytes| core::str::from_utf8(value_bytes).ok())
                .and_then(|value_text| value_text.parse::<u16>().ok());
            match parsed_port {
                Some(port_number) => println!("OK  port as u16 = {}", port_number),
                None => println!("ERR port not_a_u16"),
            }
        }
        Err(error_code) => println!("ERR port code {}", error_code.code()),
    }

    // Path: read into a path-sized buffer, then text, then Path (must be absolute).
    match read_single_line_string_field_from_toml_no_heap::<4096>(
        absolute_toml_file_path,
        "data_directory",
    ) {
        Ok((output_buffer, written_length)) => {
            let value_text = output_buffer
                .get(..written_length)
                .and_then(|value_bytes| core::str::from_utf8(value_bytes).ok());
            match value_text {
                Some(path_text) => {
                    let data_directory = Path::new(path_text);
                    if data_directory.is_absolute() {
                        println!("OK  data_directory as Path = {}", data_directory.display());
                    } else {
                        println!("ERR data_directory not_absolute");
                    }
                }
                None => println!("ERR data_directory non_utf8_value"),
            }
        }
        Err(error_code) => println!("ERR data_directory code {}", error_code.code()),
    }
}

/// Reads one field and prints one fixed-format line.
///
/// - Success: `OK  <key> = <value>` (or `ERR <key> non_utf8_value`).
/// - Failure: `ERR <key> code <n>`, numeric code only.
///
/// Demo policy: "field not found" (811) continues; any other error exits
/// with `EXIT_READ_FAILED`. Production callers apply their own recovery
/// policy per code and do not terminate.
fn print_field_or_terse_error(target_field_key: &str, absolute_toml_file_path: &Path) {
    let read_result = read_single_line_string_field_from_toml_no_heap::<DEMO_OUTPUT_BUFFER_BYTES>(
        absolute_toml_file_path,
        target_field_key,
    );
    match read_result {
        Ok((output_buffer, written_length)) => {
            // UTF-8 validation is the caller's decision, made here for printing.
            let value_text = output_buffer
                .get(..written_length)
                .and_then(|value_bytes| core::str::from_utf8(value_bytes).ok());
            match value_text {
                Some(value_as_str) => println!("OK  {} = {}", target_field_key, value_as_str),
                None => println!("ERR {} non_utf8_value", target_field_key),
            }
        }
        Err(error_code) => {
            println!("ERR {} code {}", target_field_key, error_code.code());
            if error_code != ReadTomlFieldError::RsLsfFieldNotFound {
                std::process::exit(EXIT_READ_FAILED);
            }
        }
    }
}

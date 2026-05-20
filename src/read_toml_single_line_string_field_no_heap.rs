//! ============================================================================
//! Module: read_toml_single_line_string_field_no_heap
//! ============================================================================
//!
//! # Project Context (Strategic Scope)
//!
//! Many production deployments need to read a handful of short configuration
//! values (e.g. a node identifier, a mode flag, a short key fingerprint) from
//! a TOML file at startup. The standard-library idiom
//! `BufReader::new(File).lines()` is unsuitable for several production
//! contexts because it:
//!
//!   * Heap-allocates a buffer (~8 KiB) inside `BufReader`.
//!   * Heap-allocates a fresh `String` for every line via `lines()`.
//!   * Returns owned `String` values, propagating heap use to the caller.
//!
//! Heap allocation in production hot paths or early-boot paths is undesirable
//! because it: enlarges attack surface (allocator bugs, OOM-as-DoS), defeats
//! static memory budgeting, complicates real-time guarantees, and obscures the
//! true memory footprint of the program.
//!
//! This module exposes a single function,
//! [`read_single_line_string_field_from_toml_no_heap`], that reads a single
//! short-string field from a TOML file using only stack-allocated buffers.
//!
//! # In Scope
//!
//! * One key per call, top-level (no `[section]`).
//! * Single-line values up to a caller-chosen `OUTPUT_BUFFER_BYTES` length.
//! * Values quoted with simple double quotes (`"..."`) or unquoted (numbers,
//!   bare identifiers).
//! * Lines using LF or CRLF terminators.
//! * Lines beginning with `#` (after trimming) are treated as comments.
//!
//! # Explicitly Out Of Scope (Non-Goals)
//!
//! * Full TOML grammar (no arrays, tables, inline tables, multi-line strings,
//!   escape sequences, dotted keys, datetimes).
//! * Re-encoding the value (caller decides whether to `core::str::from_utf8`).
//! * Trailing inline comments on the same line as the value
//!   (e.g. `name = "x"  # note` — the `# note` becomes part of the value).
//!   If your project relies on inline comments, switch to a real parser.
//! * UTF-8 BOM at file start (the BOM bytes will appear in the first line's
//!   key prefix and cause a non-match for that key). Strip BOMs upstream if
//!   your toolchain may produce them.
//!
//! # Defensive Policy
//!
//! On any malformed input, I/O error, oversize line, oversize value, or
//! exhausted safety budget, the function returns a terse zero-data
//! [`ReadTomlFieldError`] variant. It never panics, never allocates, and
//! never includes the file path, file contents, or OS error string in the
//! returned error. Callers that need richer diagnostics should add them
//! in `#[cfg(debug_assertions)]`-gated code, not in production.
//!
//! # Concurrency
//!
//! The function is synchronous and self-contained. It does not share state.
//! It is safe to call from multiple threads with distinct paths.
//! ============================================================================

/*
Example use, main.rs code

//! ============================================================================
//! Binary: rslsf_demo
//! ----------------------------------------------------------------------------
//! Demonstrates and smoke-tests `read_single_line_string_field_from_toml_no_heap`
//! against a real on-disk TOML file.
//!
//! # Project Context
//! This binary is intentionally minimal. Its job is to:
//!   1. Write a small known TOML file to a deterministic absolute path.
//!   2. Read a single short field from it using the no-heap reader.
//!   3. Print a terse, fixed-set status — no path, no value contents in error
//!      branches — matching the production logging policy of the module.
//!
//! Note: this binary uses `println!` for the *success* path only as a
//! human-facing demonstration. Real production callers should route output
//! through their own logging facility.
//! ============================================================================

/*

## 16 is not hardcoded anywhere in the module.

The `16` lives only in `main.rs`:

```rust
const DEMO_OUTPUT_BUFFER_BYTES: usize = 16;   // demo's choice, not the module's
```

and is passed to the function as a **const generic** at the call site:

```rust
read_single_line_string_field_from_toml_no_heap::<DEMO_OUTPUT_BUFFER_BYTES>(...)
//                                                ^^^^^^^^^^^^^^^^^^^^^^^^
//                                                this is the knob
```

You can call it with any compile-time size you like:

```rust
read_single_line_string_field_from_toml_no_heap::<8>(path, "x")?;
read_single_line_string_field_from_toml_no_heap::<32>(path, "x")?;
read_single_line_string_field_from_toml_no_heap::<256>(path, "x")?;
```

Each call site picks its own value buffer size. They do not interfere with each other.

## There are actually three buffers in play. Only one is caller-tunable today.

| Buffer | Purpose | Size today | Where set | Caller-tunable? |
|---|---|---|---|---|
| **Output value buffer** | Holds the extracted field value | `OUTPUT_BUFFER_BYTES` (e.g. `16`) | const generic at call site | **Yes** |
| **File read chunk** | One `file.read()` lands here | `RSLSF_READ_CHUNK_BYTES = 256` | module constant | No (today) |
| **Line accumulator** | Reassembles a single line across chunks | `RSLSF_MAX_LINE_BYTES = 512` | module constant | No (today) |

The two internal buffers (256 and 512) are fixed for every caller. The output buffer is per-call.

## Why the asymmetry, and how to change it if you want symmetry

The asymmetry exists because, in practice:

- Callers care a lot about the **output size** — it determines what value lengths they accept, and they want it as small as their data allows (no wasted stack).
- Callers usually do **not** care about the internal scan buffer sizes — those just need to be "big enough for any reasonable line."

If your project does want all three tunable (e.g. you are on a microcontroller with 8 KiB of stack and want to shrink the line accumulator to 128 bytes), promote them to const generics as well:

```rust
pub fn read_single_line_string_field_from_toml_no_heap<
    const OUTPUT_BUFFER_BYTES: usize,
    const READ_CHUNK_BYTES:    usize,
    const MAX_LINE_BYTES:      usize,
>(
    absolute_toml_file_path: &str,
    target_field_key: &str,
) -> Result<([u8; OUTPUT_BUFFER_BYTES], usize), ReadTomlFieldError> { ... }
```

Call site then becomes:

```rust
read_single_line_string_field_from_toml_no_heap::<16, 256, 512>(path, "name")?;
```

That trade is verbosity at call sites for full caller control of stack footprint. Pick whichever side you want; both are stack-only and heap-free.

*/

mod read_toml_single_line_string_field_no_heap;

use read_toml_single_line_string_field_no_heap::{
    ReadTomlFieldError, read_single_line_string_field_from_toml_no_heap,
};

use std::io::Write;

/// Size of the on-stack output buffer used in this demo.
/// Picked to comfortably hold the demo value `"alice-node-01"` (13 bytes)
/// without being wastefully large.
const DEMO_OUTPUT_BUFFER_BYTES: usize = 16;

/// Name of the demo TOML file written under the OS temp directory.
const DEMO_TOML_FILE_NAME: &str = "rslsf_demo_config.toml";

/// Contents of the demo TOML file. Kept simple and inside the in-scope
/// subset documented by the reader module (no sections, no escapes,
/// no multi-line strings).
const DEMO_TOML_FILE_CONTENTS: &str = "\
# rslsf_demo: sample configuration
# This file is overwritten on every run.

node_id   = \"alice-node-01\"
mode      = \"production\"
port      = 8080
";

/// Process exit codes are kept few and fixed to avoid leaking detail.
/// Production policy: never propagate raw OS errors to the exit code.
const EXIT_OK: i32 = 0;
const EXIT_DEMO_SETUP_FAILED: i32 = 10;
const EXIT_READ_FAILED: i32 = 20;

fn main() {
    // ------------------------------------------------------------------
    // Step 1: write the demo TOML file to an absolute path under temp_dir.
    // This step is the demo's responsibility — it is NOT a production
    // pattern, just a way to give the reader something to read.
    // ------------------------------------------------------------------
    let mut demo_file_absolute_path = std::env::temp_dir();
    demo_file_absolute_path.push(DEMO_TOML_FILE_NAME);

    match std::fs::File::create(&demo_file_absolute_path)
        .and_then(|mut f| f.write_all(DEMO_TOML_FILE_CONTENTS.as_bytes()))
    {
        Ok(()) => {}
        Err(_) => {
            // Terse message: do NOT print the path or OS error.
            eprintln!("rslsf_demo: setup failed");
            std::process::exit(EXIT_DEMO_SETUP_FAILED);
        }
    }

    // Convert PathBuf to &str for the reader (which takes &str).
    // If the temp dir has a non-UTF-8 path on this platform, we bail
    // cleanly without exposing the path.
    let demo_file_path_as_str: &str = match demo_file_absolute_path.to_str() {
        Some(s) => s,
        None => {
            eprintln!("rslsf_demo: setup failed");
            std::process::exit(EXIT_DEMO_SETUP_FAILED);
        }
    };

    // ------------------------------------------------------------------
    // Step 2: read three fields and print results in a fixed-format way.
    // We deliberately call the reader three times to demonstrate that
    // each call is independent and reuses no state.
    // ------------------------------------------------------------------
    print_field_or_terse_error("node_id", demo_file_path_as_str);
    print_field_or_terse_error("mode", demo_file_path_as_str);
    print_field_or_terse_error("port", demo_file_path_as_str);
    print_field_or_terse_error("missing_key", demo_file_path_as_str);

    std::process::exit(EXIT_OK);
}

/// Read one field and print a single fixed-format line.
///
/// On success the line is `OK <key> = <value>`.
/// On any error the line is `ERR <key> <error-code>` with NO path,
/// NO file contents, NO OS error text — matching the module's policy.
fn print_field_or_terse_error(target_field_key: &str, absolute_toml_file_path: &str) {
    let read_result = read_single_line_string_field_from_toml_no_heap::<DEMO_OUTPUT_BUFFER_BYTES>(
        absolute_toml_file_path,
        target_field_key,
    );

    match read_result {
        Ok((output_buffer, written_length)) => {
            // Validate UTF-8 at the boundary, since we want to print as text.
            // This validation is the caller's responsibility, not the reader's.
            match core::str::from_utf8(&output_buffer[..written_length]) {
                Ok(value_as_str) => {
                    println!("OK  {} = {}", target_field_key, value_as_str);
                }
                Err(_) => {
                    // Value is not UTF-8: report terse, do not print bytes.
                    println!("ERR {} non_utf8_value", target_field_key);
                }
            }
        }
        Err(error_variant) => {
            println!(
                "ERR {} {}",
                target_field_key,
                terse_error_code(error_variant),
            );

            // Demo policy: if the failure is something other than
            // "field not found" we treat it as a hard failure and
            // exit. In real production code, the caller would handle
            // each variant according to its own recovery policy and
            // would NOT terminate the program.
            match error_variant {
                ReadTomlFieldError::RsLsfFieldNotFound => {
                    // expected for "missing_key"; keep going.
                }
                _ => {
                    std::process::exit(EXIT_READ_FAILED);
                }
            }
        }
    }
}

/// Map each error variant to a short, fixed, log-safe code.
///
/// These codes are stable strings the operator can grep for. They contain
/// no path, no contents, and no OS detail — by design.
fn terse_error_code(error_variant: ReadTomlFieldError) -> &'static str {
    match error_variant {
        ReadTomlFieldError::RsLsfEmptyKey => "E_EMPTY_KEY",
        ReadTomlFieldError::RsLsfKeyTooLong => "E_KEY_TOO_LONG",
        ReadTomlFieldError::RsLsfOutputBufferZeroSized => "E_OUTBUF_ZERO",
        ReadTomlFieldError::RsLsfFileOpenFailed => "E_OPEN",
        ReadTomlFieldError::RsLsfFileReadFailed => "E_READ",
        ReadTomlFieldError::RsLsfFieldNotFound => "E_NOT_FOUND",
        ReadTomlFieldError::RsLsfValueExceedsOutputBuffer => "E_VALUE_TOO_BIG",
        ReadTomlFieldError::RsLsfMatchingLineExceedsScanBuffer => "E_LINE_TOO_BIG",
        ReadTomlFieldError::RsLsfSafetyBudgetExhausted => "E_SAFETY_BUDGET",
    }
}



*/

use std::fs::File;
use std::io::Read;

// ----------------------------------------------------------------------------
// Module constants
// ----------------------------------------------------------------------------

/// Stack-allocated chunk size used for `File::read`.
///
/// Tradeoff: smaller chunks reduce stack pressure; larger chunks reduce syscall
/// count. 256 B is comfortable on every realistic stack and keeps syscall
/// overhead acceptable for the small-config use case this module targets.
const RSLSF_READ_CHUNK_BYTES: usize = 16;

/// Maximum bytes accumulated for a single line during scanning (stack-only).
///
/// Lines exceeding this limit do NOT silently truncate; see overflow handling
/// in [`read_single_line_string_field_from_toml_no_heap`]. 512 B comfortably
/// covers any realistic single-line TOML key/value in the in-scope subset.
pub const RSLSF_MAX_LINE_BYTES: usize = 32;

/// Failsafe upper bound on total bytes scanned from a single file.
///
/// Bounds the read loop even if the OS keeps returning data (NASA P10 rule 2).
/// Tune for your project's expected configuration size. 1 MiB is generous for
/// configuration files while preventing pathological/adversarial inputs from
/// running unbounded work.
pub const RSLSF_MAX_BYTES_SCANNED: u64 = 1 << 20;

// ----------------------------------------------------------------------------
// Error type
// ----------------------------------------------------------------------------

/// Production-safe error type for
/// [`read_single_line_string_field_from_toml_no_heap`].
///
/// # Design
///
/// * All variants are zero-sized: no heap, no `String`, no embedded path,
///   no embedded OS error. This is a deliberate defensive choice — error
///   values must never become an information-disclosure vector.
/// * Every variant carries the unique prefix `RsLsf` (Read Single Line
///   String Field) so it is unambiguously traceable in logs to this
///   function, satisfying the "unique error per function" rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadTomlFieldError {
    /// RSLSF: caller-supplied key was empty.
    RsLsfEmptyKey,
    /// RSLSF: caller-supplied key would not fit in the line scan buffer.
    RsLsfKeyTooLong,
    /// RSLSF: caller-supplied `OUTPUT_BUFFER_BYTES` const-generic was zero.
    RsLsfOutputBufferZeroSized,
    /// RSLSF: the file could not be opened (does not exist, permission, etc.).
    RsLsfFileOpenFailed,
    /// RSLSF: an I/O error occurred while reading.
    RsLsfFileReadFailed,
    /// RSLSF: the requested key was not present in the file.
    RsLsfFieldNotFound,
    /// RSLSF: the matched value would not fit in `OUTPUT_BUFFER_BYTES`.
    RsLsfValueExceedsOutputBuffer,
    /// RSLSF: a line whose leading bytes matched the requested key exceeded
    /// `RSLSF_MAX_LINE_BYTES`; refusing to silently truncate.
    RsLsfMatchingLineExceedsScanBuffer,
    /// RSLSF: the failsafe byte/iteration budget was exhausted.
    RsLsfSafetyBudgetExhausted,
}

// ----------------------------------------------------------------------------
// Public API
// ----------------------------------------------------------------------------

/// Reads a single top-level single-line string field from a TOML file using
/// only stack-allocated memory (buffer size set by call to function).
///
/// e.g.
/// read_single_line_string_field_from_toml_no_heap::<8>(path, "x")?;
/// read_single_line_string_field_from_toml_no_heap::<32>(path, "x")?;
/// read_single_line_string_field_from_toml_no_heap::<256>(path, "x")?;
///
/// # Type Parameters
/// * `OUTPUT_BUFFER_BYTES` — the fixed size of the returned byte buffer.
///   Must be `> 0`. Pick the smallest value that comfortably fits your
///   project's value (e.g. `16` for a short identifier).
///
/// # Arguments
/// * `absolute_toml_file_path` — absolute path to the TOML file. Relative
///   paths technically work but are discouraged per project policy because
///   they depend on the current working directory.
/// * `target_field_key` — the exact top-level key to find. Must be non-empty
///   and shorter than [`RSLSF_MAX_LINE_BYTES`].
///
/// # Returns
/// * `Ok((output_buffer, written_length))` on success. `output_buffer` is
///   `[u8; OUTPUT_BUFFER_BYTES]`, and `written_length` bytes of it are
///   meaningful. Bytes past `written_length` are zero.
/// * `Err(ReadTomlFieldError)` on any failure; never panics, never allocates.
///
/// # Example (illustrative)
/// ```ignore
/// match read_single_line_string_field_from_toml_no_heap::<16>(
///     "/etc/myapp/config.toml",
///     "node_id",
/// ) {
///     Ok((buf, len)) => {
///         // Caller decides whether to validate UTF-8.
///         if let Ok(s) = core::str::from_utf8(&buf[..len]) {
///             // use s
///         }
///     }
///     Err(_e) => {
///         // Production: log a unique short code, do NOT log path/contents.
///         // Continue with safe default; do not panic.
///     }
/// }
/// ```
/// # Type Parameters
/// * `OUTPUT_BUFFER_BYTES` — chosen by the caller at the call site, at
///   compile time, via the turbofish syntax `::<N>`. This is the size of
///   the value buffer returned to the caller; it is the maximum value
///   length this call will accept. There is no default and no hardcoded
///   value inside this module. Pick the smallest `N` that fits the
///   longest legitimate value your caller will accept; values longer
///   than `N` produce `RsLsfValueExceedsOutputBuffer` and are never
///   silently truncated.
///
/// # Internal buffers (not caller-tunable in this version)
/// Two internal scan buffers are sized by module-level constants:
/// [`RSLSF_READ_CHUNK_BYTES`] and [`RSLSF_MAX_LINE_BYTES`]. If your
/// project needs to tune those (e.g. tighter stack budget on embedded
/// targets), promote them to additional const generics on this function.
///
pub fn read_single_line_string_field_from_toml_no_heap<const OUTPUT_BUFFER_BYTES: usize>(
    absolute_toml_file_path: &str,
    target_field_key: &str,
) -> Result<([u8; OUTPUT_BUFFER_BYTES], usize), ReadTomlFieldError> {
    // ----------------------------------------------------------------------
    // Debug-Assert / Test-Assert / Production-Catch
    // ----------------------------------------------------------------------
    // Debug-only assertion: panics only in non-test debug builds, so
    // developers see violated preconditions during local iteration.
    #[cfg(all(debug_assertions, not(test)))]
    {
        debug_assert!(
            OUTPUT_BUFFER_BYTES > 0,
            "RSLSF: OUTPUT_BUFFER_BYTES must be > 0",
        );
        debug_assert!(
            !target_field_key.is_empty(),
            "RSLSF: target_field_key must not be empty",
        );
    }

    // Production catch-handles: never panic; convert violations into errors.
    if OUTPUT_BUFFER_BYTES == 0 {
        return Err(ReadTomlFieldError::RsLsfOutputBufferZeroSized);
    }
    if target_field_key.is_empty() {
        return Err(ReadTomlFieldError::RsLsfEmptyKey);
    }
    if target_field_key.len() >= RSLSF_MAX_LINE_BYTES {
        return Err(ReadTomlFieldError::RsLsfKeyTooLong);
    }

    // ----------------------------------------------------------------------
    // Open the file (terse error: no path leakage)
    // ----------------------------------------------------------------------
    let mut open_file_handle: File = match File::open(absolute_toml_file_path) {
        Ok(handle) => handle,
        Err(_) => return Err(ReadTomlFieldError::RsLsfFileOpenFailed),
    };

    // ----------------------------------------------------------------------
    // Stack-allocated scratch buffers
    // ----------------------------------------------------------------------
    let mut chunk_read_buffer: [u8; RSLSF_READ_CHUNK_BYTES] = [0u8; RSLSF_READ_CHUNK_BYTES];
    let mut current_line_buffer: [u8; RSLSF_MAX_LINE_BYTES] = [0u8; RSLSF_MAX_LINE_BYTES];
    let mut current_line_length: usize = 0;
    let mut current_line_overflowed_buffer: bool = false;
    let mut cumulative_bytes_scanned: u64 = 0;

    // Iteration failsafe: even if `read` were to misbehave and always return 1
    // byte, this caps the outer loop. (Belt-and-suspenders with the byte cap.)
    let mut safety_iteration_count: u64 = 0;
    let safety_iteration_limit: u64 =
        (RSLSF_MAX_BYTES_SCANNED / (RSLSF_READ_CHUNK_BYTES as u64)) + 16;

    // ----------------------------------------------------------------------
    // Read loop
    // ----------------------------------------------------------------------
    loop {
        safety_iteration_count = safety_iteration_count.saturating_add(1);
        if safety_iteration_count > safety_iteration_limit {
            return Err(ReadTomlFieldError::RsLsfSafetyBudgetExhausted);
        }

        let bytes_read_this_chunk: usize = match open_file_handle.read(&mut chunk_read_buffer) {
            Ok(count) => count,
            Err(_) => return Err(ReadTomlFieldError::RsLsfFileReadFailed),
        };

        // ------------------------------------------------------------------
        // End-of-file: process any final unterminated line, then report.
        // ------------------------------------------------------------------
        if bytes_read_this_chunk == 0 {
            if current_line_overflowed_buffer {
                // If the overflowing trailing line *could* have been our key,
                // it is unsafe to silently skip it: report explicitly.
                if line_prefix_could_match_key(
                    &current_line_buffer[..current_line_length],
                    target_field_key,
                ) {
                    return Err(ReadTomlFieldError::RsLsfMatchingLineExceedsScanBuffer);
                }
            } else if current_line_length > 0 {
                match try_match_line_against_key::<OUTPUT_BUFFER_BYTES>(
                    &current_line_buffer[..current_line_length],
                    target_field_key,
                )? {
                    Some(found_value_tuple) => return Ok(found_value_tuple),
                    None => {}
                }
            }
            return Err(ReadTomlFieldError::RsLsfFieldNotFound);
        }

        cumulative_bytes_scanned =
            cumulative_bytes_scanned.saturating_add(bytes_read_this_chunk as u64);
        if cumulative_bytes_scanned > RSLSF_MAX_BYTES_SCANNED {
            return Err(ReadTomlFieldError::RsLsfSafetyBudgetExhausted);
        }

        // ------------------------------------------------------------------
        // Byte-by-byte line accumulator. Bounded by `bytes_read_this_chunk`
        // (always <= RSLSF_READ_CHUNK_BYTES), so this inner loop is bounded.
        // ------------------------------------------------------------------
        let mut byte_index_in_chunk: usize = 0;
        while byte_index_in_chunk < bytes_read_this_chunk {
            let current_byte: u8 = chunk_read_buffer[byte_index_in_chunk];
            byte_index_in_chunk += 1;

            match current_byte {
                b'\n' => {
                    if current_line_overflowed_buffer {
                        if line_prefix_could_match_key(
                            &current_line_buffer[..current_line_length],
                            target_field_key,
                        ) {
                            return Err(ReadTomlFieldError::RsLsfMatchingLineExceedsScanBuffer);
                        }
                        // Otherwise: this unrelated long line cannot affect us;
                        // continue scanning the file.
                    } else if current_line_length > 0 {
                        match try_match_line_against_key::<OUTPUT_BUFFER_BYTES>(
                            &current_line_buffer[..current_line_length],
                            target_field_key,
                        )? {
                            Some(found_value_tuple) => return Ok(found_value_tuple),
                            None => {}
                        }
                    }
                    current_line_length = 0;
                    current_line_overflowed_buffer = false;
                }
                b'\r' => {
                    // Drop CR so CRLF and LF line endings are both handled.
                    // A bare CR (old Mac line endings) is also dropped; not
                    // a supported terminator per the in-scope policy above.
                }
                _ => {
                    if current_line_length < RSLSF_MAX_LINE_BYTES {
                        current_line_buffer[current_line_length] = current_byte;
                        current_line_length += 1;
                    } else {
                        // Overflow: stop accumulating but keep the prefix so
                        // we can decide at line end whether overflow matters.
                        current_line_overflowed_buffer = true;
                    }
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Internal helpers (pure, stateless, no heap)
// ----------------------------------------------------------------------------

/// Attempt to match a single fully-accumulated line against `target_field_key`.
///
/// Returns:
/// * `Ok(Some((buf, len)))` — line matched the key and the value fit.
/// * `Ok(None)`              — line did not match the key (skip and continue).
/// * `Err(...)`              — line matched the key but the value will not
///                              fit in `OUTPUT_BUFFER_BYTES`.
fn try_match_line_against_key<const OUTPUT_BUFFER_BYTES: usize>(
    raw_line_bytes: &[u8],
    target_field_key: &str,
) -> Result<Option<([u8; OUTPUT_BUFFER_BYTES], usize)>, ReadTomlFieldError> {
    let trimmed_line_bytes: &[u8] = trim_ascii_whitespace(raw_line_bytes);

    // Empty lines and full-line comments cannot be key-value pairs.
    if trimmed_line_bytes.is_empty() {
        return Ok(None);
    }
    if trimmed_line_bytes[0] == b'#' {
        return Ok(None);
    }

    let key_bytes: &[u8] = target_field_key.as_bytes();
    if trimmed_line_bytes.len() < key_bytes.len() {
        return Ok(None);
    }

    // The line must begin with the exact key bytes...
    if &trimmed_line_bytes[..key_bytes.len()] != key_bytes {
        return Ok(None);
    }

    // ...followed by optional whitespace and a single '='. This prevents
    // partial-prefix collisions such as key "name" against line "name_long".
    let post_key_bytes: &[u8] = &trimmed_line_bytes[key_bytes.len()..];
    let mut cursor_position: usize = 0;
    while cursor_position < post_key_bytes.len()
        && is_ascii_space_or_tab_byte(post_key_bytes[cursor_position])
    {
        cursor_position += 1;
    }
    if cursor_position >= post_key_bytes.len() || post_key_bytes[cursor_position] != b'=' {
        return Ok(None);
    }
    cursor_position += 1;
    while cursor_position < post_key_bytes.len()
        && is_ascii_space_or_tab_byte(post_key_bytes[cursor_position])
    {
        cursor_position += 1;
    }

    // Strip a single pair of surrounding double quotes, if present.
    let raw_value_bytes: &[u8] = &post_key_bytes[cursor_position..];
    let stripped_value_bytes: &[u8] = strip_surrounding_double_quotes(raw_value_bytes);

    if stripped_value_bytes.len() > OUTPUT_BUFFER_BYTES {
        return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
    }

    let mut output_buffer: [u8; OUTPUT_BUFFER_BYTES] = [0u8; OUTPUT_BUFFER_BYTES];
    output_buffer[..stripped_value_bytes.len()].copy_from_slice(stripped_value_bytes);
    Ok(Some((output_buffer, stripped_value_bytes.len())))
}

/// True iff, after leading-whitespace trim, `raw_line_bytes` starts with the
/// exact bytes of `candidate_key`. Used solely to decide whether an overflowed
/// line could have been the line we cared about.
fn line_prefix_could_match_key(raw_line_bytes: &[u8], candidate_key: &str) -> bool {
    let mut leading_index: usize = 0;
    while leading_index < raw_line_bytes.len()
        && is_ascii_space_or_tab_byte(raw_line_bytes[leading_index])
    {
        leading_index += 1;
    }
    let post_whitespace_bytes: &[u8] = &raw_line_bytes[leading_index..];
    let key_bytes: &[u8] = candidate_key.as_bytes();
    if post_whitespace_bytes.len() < key_bytes.len() {
        return false;
    }
    &post_whitespace_bytes[..key_bytes.len()] == key_bytes
}

/// Trim ASCII whitespace (space, tab, CR, LF) from both ends, without
/// allocating. Returns a sub-slice of the input.
fn trim_ascii_whitespace(input_bytes: &[u8]) -> &[u8] {
    let mut start_index: usize = 0;
    let mut end_index: usize = input_bytes.len();
    while start_index < end_index && is_ascii_whitespace_byte(input_bytes[start_index]) {
        start_index += 1;
    }
    while end_index > start_index && is_ascii_whitespace_byte(input_bytes[end_index - 1]) {
        end_index -= 1;
    }
    &input_bytes[start_index..end_index]
}

#[inline]
fn is_ascii_whitespace_byte(byte_value: u8) -> bool {
    matches!(byte_value, b' ' | b'\t' | b'\r' | b'\n')
}

#[inline]
fn is_ascii_space_or_tab_byte(byte_value: u8) -> bool {
    matches!(byte_value, b' ' | b'\t')
}

/// If `input_bytes` is at least two bytes long and both first and last bytes
/// are `"`, return the inner slice; otherwise return `input_bytes` unchanged.
/// Does not handle escape sequences — out of scope for this module.
fn strip_surrounding_double_quotes(input_bytes: &[u8]) -> &[u8] {
    if input_bytes.len() >= 2
        && input_bytes[0] == b'"'
        && input_bytes[input_bytes.len() - 1] == b'"'
    {
        &input_bytes[1..input_bytes.len() - 1]
    } else {
        input_bytes
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------
#[cfg(test)]
mod rslsf_tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    /// Helper: write a fresh temp file with the given contents and return its
    /// absolute path. Test-only; may use heap/panic-on-failure freely.
    fn write_unique_temp_toml(label: &str, contents: &str) -> PathBuf {
        let mut path_buffer = std::env::temp_dir();
        // Make each test file name unique to avoid cross-test interference
        // even when tests run in parallel.
        let unique_suffix = format!(
            "{}_{}_{}",
            std::process::id(),
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        path_buffer.push(format!("rslsf_test_{}.toml", unique_suffix));
        let mut created_file =
            std::fs::File::create(&path_buffer).expect("test setup: create temp file");
        created_file
            .write_all(contents.as_bytes())
            .expect("test setup: write temp file");
        path_buffer
    }

    fn path_as_str(path: &PathBuf) -> &str {
        path.to_str().expect("test setup: temp path must be UTF-8")
    }

    #[test]
    fn rslsf_finds_simple_quoted_value() {
        let test_path = write_unique_temp_toml("simple_quoted", "name = \"alice\"\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value");
        assert_eq!(&output_buffer[..written_length], b"alice");
    }

    #[test]
    fn rslsf_finds_unquoted_value() {
        let test_path = write_unique_temp_toml("unquoted", "port = 8080\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "port")
                .expect("should find value");
        assert_eq!(&output_buffer[..written_length], b"8080");
    }

    #[test]
    fn rslsf_handles_crlf_endings() {
        let test_path = write_unique_temp_toml("crlf", "name = \"bob\"\r\nother = \"x\"\r\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value");
        assert_eq!(&output_buffer[..written_length], b"bob");
    }

    #[test]
    fn rslsf_skips_comments_and_blank_lines() {
        let test_path = write_unique_temp_toml(
            "comments",
            "# a header comment\n\n   # indented comment\nname = \"carol\"\n",
        );
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value");
        assert_eq!(&output_buffer[..written_length], b"carol");
    }

    #[test]
    fn rslsf_does_not_match_key_with_extra_prefix_chars() {
        // "name_long" must NOT be accepted when caller asked for "name".
        let test_path = write_unique_temp_toml(
            "prefix_collision",
            "name_long = \"WRONG\"\nname = \"RIGHT\"\n",
        );
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value");
        assert_eq!(&output_buffer[..written_length], b"RIGHT");
    }

    #[test]
    fn rslsf_returns_field_not_found_when_missing() {
        let test_path = write_unique_temp_toml("missing", "other = \"x\"\n");
        let result =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name");
        assert_eq!(result, Err(ReadTomlFieldError::RsLsfFieldNotFound));
    }

    #[test]
    fn rslsf_returns_value_too_long_when_output_too_small() {
        // "toolongxx" is 9 bytes, which exceeds the 8-byte output buffer.
        let test_path = write_unique_temp_toml("too_long", "name = \"toolongxx\"\n");
        let result =
            read_single_line_string_field_from_toml_no_heap::<8>(path_as_str(&test_path), "name");
        assert_eq!(
            result,
            Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer)
        );
    }

    #[test]
    fn rslsf_returns_open_failed_for_nonexistent_path() {
        // Path under temp_dir that we have not created. Avoid hard-coding
        // platform-specific absolute paths (e.g. "/nope/...") so the test
        // is portable to Windows runners.
        let mut bogus_path = std::env::temp_dir();
        bogus_path.push("rslsf_test_definitely_does_not_exist_xyzzy_12345.toml");
        let result = read_single_line_string_field_from_toml_no_heap::<16>(
            bogus_path
                .to_str()
                .expect("test setup: temp path must be UTF-8"),
            "name",
        );
        assert_eq!(result, Err(ReadTomlFieldError::RsLsfFileOpenFailed));
    }

    #[test]
    fn rslsf_rejects_empty_key() {
        let test_path = write_unique_temp_toml("empty_key", "name = \"x\"\n");
        let result =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "");
        assert_eq!(result, Err(ReadTomlFieldError::RsLsfEmptyKey));
    }

    #[test]
    fn rslsf_rejects_key_too_long() {
        let test_path = write_unique_temp_toml("key_too_long", "name = \"x\"\n");
        // Build a key longer than RSLSF_MAX_LINE_BYTES without using `vec!`
        // anywhere in production code — this is test-only setup.
        let oversized_key: String = "k".repeat(RSLSF_MAX_LINE_BYTES + 1);
        let result = read_single_line_string_field_from_toml_no_heap::<16>(
            path_as_str(&test_path),
            &oversized_key,
        );
        assert_eq!(result, Err(ReadTomlFieldError::RsLsfKeyTooLong));
    }

    #[test]
    fn rslsf_handles_no_trailing_newline() {
        // No final '\n' — the last line must still be processed at EOF.
        let test_path = write_unique_temp_toml("no_trailing_lf", "name = \"dora\"");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value at EOF without newline");
        assert_eq!(&output_buffer[..written_length], b"dora");
    }

    #[test]
    fn rslsf_finds_key_after_many_unrelated_lines() {
        // Force the scanner to cross several read-chunk boundaries before
        // it sees the target key. This exercises the chunk/line-accumulator
        // boundary logic.
        let mut contents = String::new();
        for i in 0..50 {
            // Each line ~30 bytes; 50 lines ~ 1500 bytes, well over one chunk.
            contents.push_str(&format!("noise_key_{:03} = \"junkjunkjunk\"\n", i));
        }
        contents.push_str("target = \"eve\"\n");
        let test_path = write_unique_temp_toml("many_lines", &contents);
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(
                path_as_str(&test_path),
                "target",
            )
            .expect("should find value across chunk boundaries");
        assert_eq!(&output_buffer[..written_length], b"eve");
    }

    #[test]
    fn rslsf_unrelated_long_line_does_not_abort_scan() {
        // A line far longer than RSLSF_MAX_LINE_BYTES that is NOT the key
        // we are looking for must be silently skipped, not aborted, so the
        // real key further down in the file is still found.
        let oversized_unrelated_line: String = std::iter::once("other_key = \"")
            .chain(std::iter::repeat("X").take(RSLSF_MAX_LINE_BYTES + 64))
            .chain(std::iter::once("\"\n"))
            .collect();
        let mut contents = String::new();
        contents.push_str(&oversized_unrelated_line);
        contents.push_str("name = \"frank\"\n");
        let test_path = write_unique_temp_toml("unrelated_overflow", &contents);
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("unrelated overflowing line should not block finding the real key");
        assert_eq!(&output_buffer[..written_length], b"frank");
    }

    #[test]
    fn rslsf_matching_line_overflow_is_reported_not_truncated() {
        // The TARGET key's line overflows the scan buffer. We must NOT
        // silently truncate and return a bogus value; we must report the
        // overflow explicitly.
        let mut contents = String::new();
        contents.push_str("name = \"");
        for _ in 0..(RSLSF_MAX_LINE_BYTES + 32) {
            contents.push('Z');
        }
        contents.push_str("\"\n");
        let test_path = write_unique_temp_toml("matching_overflow", &contents);
        let result =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name");
        assert_eq!(
            result,
            Err(ReadTomlFieldError::RsLsfMatchingLineExceedsScanBuffer)
        );
    }

    #[test]
    fn rslsf_handles_whitespace_around_key_and_equals() {
        // Both leading whitespace and varied spacing around '=' must work.
        let test_path = write_unique_temp_toml("whitespace", "   name\t=\t  \"grace\"   \n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value with varied whitespace");
        assert_eq!(&output_buffer[..written_length], b"grace");
    }

    #[test]
    fn rslsf_first_match_wins_when_key_appears_twice() {
        // Document the precedence policy: first match wins. If your project
        // needs last-match-wins, change this test AND the function's docs
        // together — don't let the two drift apart.
        let test_path =
            write_unique_temp_toml("duplicate_key", "name = \"first\"\nname = \"second\"\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find first value");
        assert_eq!(&output_buffer[..written_length], b"first");
    }

    #[test]
    fn rslsf_zero_sized_output_buffer_is_rejected() {
        // We cannot exercise this via the public generic with `::<0>` and
        // get a meaningful value back, but we can confirm the production
        // catch-handle exists and triggers BEFORE any I/O occurs. We pass
        // a deliberately bogus path to prove that the parameter check fires
        // first (we get OutputBufferZeroSized, NOT FileOpenFailed).
        let mut bogus_path = std::env::temp_dir();
        bogus_path.push("rslsf_test_zero_buffer_should_not_open.toml");
        let result = read_single_line_string_field_from_toml_no_heap::<0>(
            bogus_path
                .to_str()
                .expect("test setup: temp path must be UTF-8"),
            "name",
        );
        assert_eq!(result, Err(ReadTomlFieldError::RsLsfOutputBufferZeroSized));
    }
}

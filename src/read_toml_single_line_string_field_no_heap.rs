//! # read_toml_single_line_string_field_no_heap.rs
//! Functionality: Read Single Line String Field = rslsf
//!
//! Source: https://github.com/lineality/toml_read_field_noheap_rust
//!
//! ## Purpose
//! Reads one short value for one top-level key from a small configuration
//! file written in a TOML subset, without heap allocation. Optionally checks
//! that given keys appear at most once in the file.
//!
//! ## Public functions
//! - `read_single_line_string_field_from_toml_no_heap::<N>(path, key)`:
//!   returns the value of the first line that assigns `key`, in an `N`-byte
//!   stack array, plus the number of meaningful bytes.
//! - `count_toml_key_lines_no_heap(path, key)`: number of lines that assign `key`.
//! - `check_toml_keys_are_unique_no_heap(path, keys)`: error 816 if any key in
//!   `keys` is assigned on more than one line.
//!
//! ## Why no heap, and why one byte at a time
//! The usual idiom `BufReader::new(File).lines()` heap-allocates a ~8 KiB
//! read buffer and one `String` per line, and hands owned `String`s to the
//! caller. To read one short value, this module instead:
//! - reads the file one byte per read call (no read-chunk buffer, no line buffer);
//! - compares key bytes in place against the caller's key (the key is never copied);
//! - writes only the matched value's bytes, directly into the caller's
//!   fixed-size output array.
//!
//! Bounding the output to the size the task needs is also input sanitation:
//! a value longer than the buffer cannot enter. It is refused with code 812,
//! never silently truncated.
//!
//! Cost: one read call per byte, and one file open per call. For the intended
//! use (a config file of a few dozen short lines, read a few times at startup)
//! this is negligible.
//!
//! ## Accepted file format (TOML subset)
//! - One `key = value` per line, top level only (no `[section]` support).
//! - Leading spaces and tabs are allowed, and so are spaces and tabs around `=`.
//! - Lines whose first non-whitespace byte is `#` are comments.
//! - LF or CRLF line endings.
//! - Values: double-quoted (`"..."`), or unquoted (numbers, bare words).
//! - Keys are matched exactly, byte for byte (case-sensitive). A longer key
//!   that starts with the requested key (`name_long` vs `name`) does not match.
//!
//! ## Value termination policy
//! - Quoted values end at the next `"`. End of file before it: code 813.
//! - Unquoted values end at the next `\n`; a `\r` directly before it is not
//!   part of the value. End of file before `\n`: code 813. Spaces or tabs
//!   before the `\n` ARE part of the value (the caller trims if wanted).
//! - `key =` followed directly by `\n`: an empty value (length 0), not an error.
//!
//! ## Not supported (non-goals)
//! - Full TOML: arrays, tables, inline tables, multi-line strings, escape
//!   sequences (a `\` is copied as a plain byte), dotted keys, datetimes.
//! - Trailing comments after an unquoted value: `port = 80 # x` yields the
//!   value `80 # x`. After a quoted value they are ignored.
//! - A UTF-8 byte-order mark at file start (the first key would not match).
//! - Converting the value to text: the caller decides (e.g. `str::from_utf8`).
//!
//! A quoted value that contains a newline is read across the newline (it
//! ends at `"`). Key-line counting does not know about such values, so a
//! line inside one could be counted as a key line. Files written by a
//! writer that refuses newlines in values never contain this case.
//!
//! ## Architecture
//! `scan_toml_reader_for_key` drives a finite-state machine (`LineScanState`)
//! one byte per read call, through `apply_byte_to_scan`. The scan has one of
//! two goals (`ScanGoal`): copy the first matching value, or count key lines.
//! The scanner works on any `Read`; the public functions give it an
//! unbuffered `File`, and tests give it in-memory input.
//!
//! ## Error codes
//! Codes 801-816 are used; 817-849 are reserved for the matching config
//! writer. Numbers come from the HIX config block (800-849), so they are kept
//! unchanged when this module is merged into HIX. The table is in the
//! `ReadTomlFieldError` doc comment.
//!
//! ## Modes
//! - Production-release: never panics, never allocates; errors are 2-byte codes;
//!   no printing.
//! - Debug: gated `eprintln!` diagnostics (may print the path and key);
//!   internal invariants (801, 802, 814, 815) also `debug_assert!`, gated
//!   `#[cfg(all(debug_assertions, not(test)))]`.
//! - Test: `#[cfg(test)]` unit tests using `assert!`/`unwrap`.
//!
//! ## Recovery tiers
//! - Tier 1: transient open errors are retried with bounded backoff;
//!   interrupted reads are retried within a bounded read-call allowance.
//! - Tiers 2 and 3 belong to the caller (e.g. use a default, or stop with the code).
//!
//! ## Concurrency
//! Synchronous, no shared state. Safe to call from several threads.
//!
//! #### Rust Policy, See:
//! https://github.com/lineality/rust_lang_rules
//! https://github.com/lineality/modes_and_case_handling
//! https://github.com/lineality/modes_and_case_handling/blob/main/mode_case_handling_framework_summary.md

#![forbid(unsafe_code)]

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

// ============================================================================
// Constants (every loop is bounded by these)
// ============================================================================

/// Largest file this module will scan, in bytes (1 MiB).
///
/// Far above any config file in scope. A larger file stops the scan with
/// code 810 instead of running unbounded work on unexpected input.
pub const RSLSF_MAX_BYTES_SCANNED: u64 = 1 << 20;

/// Extra read calls allowed for "interrupted" reads (the OS asks the
/// program to try again; nothing is read).
///
/// Every other read call either reads one byte or reports end of file, so
/// the byte limit already bounds those. This allowance keeps the scan loop
/// finite even if a reader reports "interrupted" forever (code 809).
const RSLSF_EXTRA_READ_CALLS: u64 = 1024;

/// Tier-1 retry: maximum attempts to open the file on transient errors.
const RSLSF_MAX_OPEN_ATTEMPTS: u32 = 3;

/// Tier-1 retry: backoff step in milliseconds (retry n sleeps n * step).
const RSLSF_OPEN_RETRY_BACKOFF_STEP_MS: u64 = 50;

// ============================================================================
// Error codes
// ============================================================================

/// Fieldless error code for this module (2 bytes, `Copy`, no heap, no PII).
///
/// Codes are append-only: never renumber or reuse.
///
/// | Code | Variant                            | Kind               | Meaning                                              |
/// |------|------------------------------------|--------------------|------------------------------------------------------|
/// | 801  | RsLsfEmptyKey                      | internal invariant | key is empty (callers pass constant keys)            |
/// | 802  | RsLsfOutputBufferZeroSized         | internal invariant | `OUTPUT_BUFFER_BYTES` is 0                           |
/// | 803  | RsLsfPathNotAbsolute               | input validation   | path is not absolute                                 |
/// | 804  | RsLsfOpenNotFound                  | input validation   | file not found                                       |
/// | 805  | RsLsfOpenPermissionDenied          | input validation   | file permission denied                               |
/// | 806  | RsLsfOpenTransient                 | transient          | transient open error, retries exhausted (retryable)  |
/// | 807  | RsLsfOpenOther                     | input validation   | any other open failure                               |
/// | 808  | RsLsfReadFailed                    | input validation   | a read failed (other than "interrupted")             |
/// | 809  | RsLsfReadCallLimitReached          | input validation   | read-call allowance used up (endless interruptions)  |
/// | 810  | RsLsfFileExceedsScanLimit          | input validation   | file larger than `RSLSF_MAX_BYTES_SCANNED`           |
/// | 811  | RsLsfFieldNotFound                 | input validation   | no line assigns the key                              |
/// | 812  | RsLsfValueExceedsOutputBuffer      | input validation   | value longer than the output buffer (no truncation)  |
/// | 813  | RsLsfValueUnterminatedAtEndOfFile  | input validation   | end of file inside a value (no `"` or no `\n`)       |
/// | 814  | RsLsfKeyMatchIndexInvalid          | internal invariant | key-match position outside the key                   |
/// | 815  | RsLsfScanOutcomeMismatch           | internal invariant | scan returned the other goal's result                |
/// | 816  | RsLsfDuplicateKey                  | input validation   | a checked key is assigned on more than one line      |
///
/// 817-849: reserved for the config writer.
// `Debug` exists only in test and debug builds, so a production-release
// binary contains no variant-name strings (intended guard).
#[cfg_attr(any(test, debug_assertions), derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ReadTomlFieldError {
    RsLsfEmptyKey = 801,
    RsLsfOutputBufferZeroSized = 802,
    RsLsfPathNotAbsolute = 803,
    RsLsfOpenNotFound = 804,
    RsLsfOpenPermissionDenied = 805,
    RsLsfOpenTransient = 806,
    RsLsfOpenOther = 807,
    RsLsfReadFailed = 808,
    RsLsfReadCallLimitReached = 809,
    RsLsfFileExceedsScanLimit = 810,
    RsLsfFieldNotFound = 811,
    RsLsfValueExceedsOutputBuffer = 812,
    RsLsfValueUnterminatedAtEndOfFile = 813,
    RsLsfKeyMatchIndexInvalid = 814,
    RsLsfScanOutcomeMismatch = 815,
    RsLsfDuplicateKey = 816,
}

impl ReadTomlFieldError {
    /// Numeric code (stable, append-only). Safe to print or log in any mode.
    pub fn code(self) -> u16 {
        self as u16
    }

    /// Tier-1 policy: may the caller retry the same bounded operation?
    /// Defined per code, with no wildcard, so a new code forces a decision.
    pub fn is_retryable(self) -> bool {
        match self {
            ReadTomlFieldError::RsLsfOpenTransient => true,

            ReadTomlFieldError::RsLsfEmptyKey
            | ReadTomlFieldError::RsLsfOutputBufferZeroSized
            | ReadTomlFieldError::RsLsfPathNotAbsolute
            | ReadTomlFieldError::RsLsfOpenNotFound
            | ReadTomlFieldError::RsLsfOpenPermissionDenied
            | ReadTomlFieldError::RsLsfOpenOther
            | ReadTomlFieldError::RsLsfReadFailed
            | ReadTomlFieldError::RsLsfReadCallLimitReached
            | ReadTomlFieldError::RsLsfFileExceedsScanLimit
            | ReadTomlFieldError::RsLsfFieldNotFound
            | ReadTomlFieldError::RsLsfValueExceedsOutputBuffer
            | ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile
            | ReadTomlFieldError::RsLsfKeyMatchIndexInvalid
            | ReadTomlFieldError::RsLsfScanOutcomeMismatch
            | ReadTomlFieldError::RsLsfDuplicateKey => false,
        }
    }
}

/// Human-readable descriptions: compiled only in debug and test builds.
#[cfg(any(debug_assertions, test))]
impl std::fmt::Display for ReadTomlFieldError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let description: &str = match self {
            ReadTomlFieldError::RsLsfEmptyKey => "key is empty",
            ReadTomlFieldError::RsLsfOutputBufferZeroSized => "OUTPUT_BUFFER_BYTES is 0",
            ReadTomlFieldError::RsLsfPathNotAbsolute => "toml path is not absolute",
            ReadTomlFieldError::RsLsfOpenNotFound => "toml file not found",
            ReadTomlFieldError::RsLsfOpenPermissionDenied => "toml file permission denied",
            ReadTomlFieldError::RsLsfOpenTransient => {
                "toml file open: transient error (retries exhausted)"
            }
            ReadTomlFieldError::RsLsfOpenOther => "toml file open failed",
            ReadTomlFieldError::RsLsfReadFailed => "toml file read failed",
            ReadTomlFieldError::RsLsfReadCallLimitReached => {
                "read-call allowance used up (reads kept being interrupted)"
            }
            ReadTomlFieldError::RsLsfFileExceedsScanLimit => {
                "toml file exceeds RSLSF_MAX_BYTES_SCANNED"
            }
            ReadTomlFieldError::RsLsfFieldNotFound => "no line assigns the key",
            ReadTomlFieldError::RsLsfValueExceedsOutputBuffer => {
                "value longer than the output buffer; refusing to truncate"
            }
            ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile => {
                "end of file inside a value (no closing quote or no newline)"
            }
            ReadTomlFieldError::RsLsfKeyMatchIndexInvalid => {
                "key-match position outside the key (internal invariant)"
            }
            ReadTomlFieldError::RsLsfScanOutcomeMismatch => {
                "scan returned the other goal's result (internal invariant)"
            }
            ReadTomlFieldError::RsLsfDuplicateKey => {
                "a checked key is assigned on more than one line"
            }
        };
        write!(formatter, "RSLSF-{}: {}", self.code(), description)
    }
}

// ============================================================================
// Scanner types
// ============================================================================

/// State of the byte-at-a-time line scanner. One state per position class
/// on a line; the current state plus the next byte decides the next state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineScanState {
    /// At the start of the file or just after `\n`.
    AtLineStart,
    /// Inside leading spaces/tabs; same transitions as `AtLineStart`.
    SkippingLeadingWhitespace,
    /// The first `matched_key_bytes` bytes of the key have matched.
    MatchingKey { matched_key_bytes: usize },
    /// The whole key matched; expecting optional spaces/tabs, then `=`.
    AwaitingEquals,
    /// Copy goal only: saw `=`; skipping spaces/tabs before the value.
    AwaitingValueStart,
    /// Copy goal only: inside an unquoted value; ends at `\n`.
    CopyingUnquotedValue,
    /// Copy goal only: inside a quoted value; ends at `"`.
    CopyingQuotedValue,
    /// This line cannot (or no longer needs to) match; skip to `\n`.
    SkippingToEndOfLine,
    /// Comment line (`#` first); skip to `\n`.
    InCommentToEndOfLine,
}

/// What one scan is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanGoal {
    /// Copy the value of the first line that assigns the key, then stop.
    CopyFirstValue,
    /// Count every line that assigns the key; read to the end of the file.
    CountKeyLines,
}

/// Result of a completed scan; the variant matches the `ScanGoal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanOutcome {
    /// Copy goal: the first `written_length` bytes of the output buffer hold the value.
    ValueCopied { written_length: usize },
    /// Count goal: number of lines that assign the key.
    KeyLinesCounted { key_line_count: u32 },
}

/// Everything one scan carries from byte to byte. Fixed size; no heap.
#[derive(Debug)]
struct ScanContext<'key, 'buffer> {
    /// The key, compared in place (never copied).
    key_bytes: &'key [u8],
    scan_goal: ScanGoal,
    /// Caller's output buffer (empty for the count goal).
    output_buffer: &'buffer mut [u8],
    state: LineScanState,
    /// Next write position in `output_buffer` (= value bytes written so far).
    write_cursor: usize,
    /// Unquoted values: a `\r` was seen and is held until the next byte
    /// shows whether it is part of a CRLF ending (dropped) or of the value
    /// (written). Avoids needing a look-ahead buffer.
    unquoted_value_has_pending_cr: bool,
    /// Count goal: lines that assign the key so far. Cannot reach
    /// `u32::MAX`: the file is capped at `RSLSF_MAX_BYTES_SCANNED` bytes.
    key_line_count: u32,
}

impl ScanContext<'_, '_> {
    /// Writes one value byte at `write_cursor` and advances it.
    ///
    /// # Returns
    /// - `Ok(())`.
    /// - `Err(RsLsfValueExceedsOutputBuffer)` (input validation): the value is
    ///   longer than the output buffer. Nothing is truncated; the caller gets
    ///   no partial value.
    fn push_value_byte(&mut self, value_byte: u8) -> Result<(), ReadTomlFieldError> {
        match self.output_buffer.get_mut(self.write_cursor) {
            Some(buffer_slot) => *buffer_slot = value_byte,
            None => {
                #[cfg(debug_assertions)]
                eprintln!(
                    "RSLSF-{}: value longer than output buffer of {} bytes",
                    ReadTomlFieldError::RsLsfValueExceedsOutputBuffer.code(),
                    self.output_buffer.len()
                );
                return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
            }
        }
        // Cannot overflow: write_cursor < output_buffer.len() was just confirmed.
        self.write_cursor = self.write_cursor.saturating_add(1);
        Ok(())
    }
}

// ============================================================================
// Public functions
// ============================================================================

/// Reads the value of the first line that assigns `target_field_key`,
/// using only stack memory.
///
/// # Project context
/// For reading short config values (paths, names, numbers) at startup with
/// no heap use. The file is read one byte per read call, and only the
/// matched value's bytes are stored. See the module doc for the accepted
/// format and the value-termination policy.
///
/// # Type parameter
/// - `OUTPUT_BUFFER_BYTES`: size of the returned array; the longest value
///   accepted. Must be > 0. Choose the smallest size that fits the field
///   (e.g. 4096 for a Linux path). Longer values return 812, never a
///   truncated value.
///
/// # Arguments
/// - `absolute_toml_file_path`: must be absolute.
/// - `target_field_key`: exact top-level key; must not be empty.
///
/// # Returns
/// - `Ok((buffer, written_length))`: `buffer[..written_length]` is the value;
///   later bytes are 0.
/// - `Err`: 801, 802 (internal invariants); 803-813 (see the error table); 815.
///
/// If the key may appear more than once and that must be an error, call
/// `check_toml_keys_are_unique_no_heap` first; this function returns the first match.
///
/// # Modes
/// Debug builds print diagnostics (may include path and key); 801 and 802
/// also panic in debug builds (not test). Production-release returns codes only.
pub fn read_single_line_string_field_from_toml_no_heap<const OUTPUT_BUFFER_BYTES: usize>(
    absolute_toml_file_path: &Path,
    target_field_key: &str,
) -> Result<([u8; OUTPUT_BUFFER_BYTES], usize), ReadTomlFieldError> {
    // --- Check: output buffer has room for at least one byte ---
    // internal invariant: callers choose a constant size > 0.
    if OUTPUT_BUFFER_BYTES == 0 {
        #[cfg(all(debug_assertions, not(test)))]
        debug_assert!(
            false,
            "RSLSF-{}: OUTPUT_BUFFER_BYTES is 0",
            ReadTomlFieldError::RsLsfOutputBufferZeroSized.code()
        );
        #[cfg(debug_assertions)]
        eprintln!(
            "RSLSF-{}: OUTPUT_BUFFER_BYTES is 0",
            ReadTomlFieldError::RsLsfOutputBufferZeroSized.code()
        );
        return Err(ReadTomlFieldError::RsLsfOutputBufferZeroSized);
    }

    let mut toml_file = open_toml_file_for_scan(absolute_toml_file_path, target_field_key)?;
    let mut output_buffer: [u8; OUTPUT_BUFFER_BYTES] = [0u8; OUTPUT_BUFFER_BYTES];
    let scan_outcome = scan_toml_reader_for_key(
        &mut toml_file,
        target_field_key.as_bytes(),
        ScanGoal::CopyFirstValue,
        &mut output_buffer,
    )?;
    let written_length = copied_value_length_from_outcome(scan_outcome)?;
    Ok((output_buffer, written_length))
}

/// Counts the lines that assign `target_field_key`.
///
/// # Project context
/// Basis of the optional duplicate-key check. A hand-edited config with a
/// key written twice would otherwise silently use the first value.
///
/// A key line is a line whose first non-whitespace bytes are exactly the
/// key, followed by optional spaces/tabs and `=`. Comment lines and longer
/// keys starting with the same bytes are not counted. See the module doc for
/// the multi-line quoted-value limitation.
///
/// # Returns
/// - `Ok(count)`: 0 if the key is not assigned.
/// - `Err`: 801, 803-810, 814, 815 (see the error table).
pub fn count_toml_key_lines_no_heap(
    absolute_toml_file_path: &Path,
    target_field_key: &str,
) -> Result<u32, ReadTomlFieldError> {
    let mut toml_file = open_toml_file_for_scan(absolute_toml_file_path, target_field_key)?;
    // The count goal never writes value bytes.
    let mut no_value_buffer: [u8; 0] = [];
    let scan_outcome = scan_toml_reader_for_key(
        &mut toml_file,
        target_field_key.as_bytes(),
        ScanGoal::CountKeyLines,
        &mut no_value_buffer,
    )?;
    key_line_count_from_outcome(scan_outcome)
}

/// Checks that each key in `target_field_keys` is assigned on at most one line.
///
/// # Project context
/// Optional pre-check before reading fields, so that an ambiguous config
/// (a key written twice) stops with a code instead of silently using the
/// first value. Keys not in the list are not checked. A key that is missing
/// is not an error here; the read function reports that (811).
///
/// Opens and scans the file once per key (bounded by the slice length).
///
/// # Returns
/// - `Ok(())`: every listed key appears at most once.
/// - `Err(RsLsfDuplicateKey)` (816, input validation): some key appears more
///   than once. The code carries no key; a caller that needs to know which key
///   can call `count_toml_key_lines_no_heap` per key. Debug builds print the key.
/// - Any error from `count_toml_key_lines_no_heap`, unchanged.
pub fn check_toml_keys_are_unique_no_heap(
    absolute_toml_file_path: &Path,
    target_field_keys: &[&str],
) -> Result<(), ReadTomlFieldError> {
    for target_field_key in target_field_keys.iter() {
        let key_line_count =
            count_toml_key_lines_no_heap(absolute_toml_file_path, target_field_key)?;
        if key_line_count > 1 {
            #[cfg(debug_assertions)]
            eprintln!(
                "RSLSF-{}: key {:?} assigned on {} lines in {}",
                ReadTomlFieldError::RsLsfDuplicateKey.code(),
                target_field_key,
                key_line_count,
                absolute_toml_file_path.display()
            );
            return Err(ReadTomlFieldError::RsLsfDuplicateKey);
        }
    }
    Ok(())
}

// ============================================================================
// File opening
// ============================================================================

/// Shared preamble of the public functions: checks the key and path, then
/// opens the file (Tier-1 retry).
///
/// # Returns
/// - `Ok(File)`: unbuffered on purpose (one byte per read call; no heap buffer).
/// - `Err(RsLsfEmptyKey)` (internal invariant: callers pass constant keys).
/// - `Err(RsLsfPathNotAbsolute)` (input validation).
/// - `Err(804 | 805 | 806 | 807)` from `open_toml_file_with_retry`.
fn open_toml_file_for_scan(
    absolute_toml_file_path: &Path,
    target_field_key: &str,
) -> Result<File, ReadTomlFieldError> {
    // --- Check: key is not empty ---
    // internal invariant: an empty key can never match and is a caller bug.
    if target_field_key.is_empty() {
        #[cfg(all(debug_assertions, not(test)))]
        debug_assert!(
            false,
            "RSLSF-{}: empty key",
            ReadTomlFieldError::RsLsfEmptyKey.code()
        );
        #[cfg(debug_assertions)]
        eprintln!(
            "RSLSF-{}: empty key",
            ReadTomlFieldError::RsLsfEmptyKey.code()
        );
        return Err(ReadTomlFieldError::RsLsfEmptyKey);
    }

    // --- Check: path is absolute ---
    // input validation: the path comes from the caller's configuration.
    if !absolute_toml_file_path.is_absolute() {
        #[cfg(debug_assertions)]
        eprintln!(
            "RSLSF-{}: not absolute: {}",
            ReadTomlFieldError::RsLsfPathNotAbsolute.code(),
            absolute_toml_file_path.display()
        );
        return Err(ReadTomlFieldError::RsLsfPathNotAbsolute);
    }

    open_toml_file_with_retry(absolute_toml_file_path)
}

/// Maps an open-error kind to this module's codes. The `io::Error` itself
/// (which may hold path text) is dropped by the caller; only the kind is used.
/// Kinds not listed are "other" (807), which is non-retryable by policy.
fn classify_toml_open_error(io_error_kind: std::io::ErrorKind) -> ReadTomlFieldError {
    match io_error_kind {
        std::io::ErrorKind::NotFound => ReadTomlFieldError::RsLsfOpenNotFound,
        std::io::ErrorKind::PermissionDenied => ReadTomlFieldError::RsLsfOpenPermissionDenied,
        std::io::ErrorKind::Interrupted
        | std::io::ErrorKind::WouldBlock
        | std::io::ErrorKind::TimedOut => ReadTomlFieldError::RsLsfOpenTransient,
        // Required wildcard: io::ErrorKind is #[non_exhaustive].
        _ => ReadTomlFieldError::RsLsfOpenOther,
    }
}

/// Opens the file read-only, retrying transient failures (Tier 1).
///
/// Up to `RSLSF_MAX_OPEN_ATTEMPTS` attempts; retry n sleeps
/// n * `RSLSF_OPEN_RETRY_BACKOFF_STEP_MS`. Non-retryable codes return at once.
///
/// # Returns
/// - `Ok(File)`.
/// - `Err(804 | 805 | 806 | 807)`.
fn open_toml_file_with_retry(absolute_toml_file_path: &Path) -> Result<File, ReadTomlFieldError> {
    for attempt_index in 0..RSLSF_MAX_OPEN_ATTEMPTS {
        match File::open(absolute_toml_file_path) {
            Ok(opened_file) => return Ok(opened_file),
            Err(io_error) => {
                let error_code = classify_toml_open_error(io_error.kind());
                #[cfg(debug_assertions)]
                eprintln!(
                    "RSLSF-{}: open attempt {} failed for {}: {}",
                    error_code.code(),
                    attempt_index,
                    absolute_toml_file_path.display(),
                    io_error
                );
                if !error_code.is_retryable() {
                    return Err(error_code);
                }
                // Last attempt failed: the result is decided, so do not sleep.
                let next_attempt_index = attempt_index.saturating_add(1);
                if next_attempt_index >= RSLSF_MAX_OPEN_ATTEMPTS {
                    return Err(error_code);
                }
                let backoff_milliseconds =
                    RSLSF_OPEN_RETRY_BACKOFF_STEP_MS.saturating_mul(u64::from(next_attempt_index));
                std::thread::sleep(Duration::from_millis(backoff_milliseconds));
            }
        }
    }
    // Reached only if RSLSF_MAX_OPEN_ATTEMPTS is 0 (no attempt was made).
    Err(ReadTomlFieldError::RsLsfOpenTransient)
}

// ============================================================================
// Scanner
// ============================================================================

/// Scans `toml_reader` one byte per read call for lines that assign the key.
///
/// # Steps (bounded loop)
/// 1. Read one byte. "Interrupted": try again (uses the extra allowance).
///    Any other read error: 808.
/// 2. End of input: finish according to goal and state
///    (`finish_scan_at_end_of_input`).
/// 3. Count the byte; more than `RSLSF_MAX_BYTES_SCANNED` bytes: 810.
/// 4. Apply the byte to the state machine (`apply_byte_to_scan`); stop if
///    the copy goal has its value.
///
/// Loop bound: at most `RSLSF_MAX_BYTES_SCANNED + 1` byte reads (the last
/// one triggers 810) plus one end-of-input read, plus
/// `RSLSF_EXTRA_READ_CALLS` interrupted reads. Reaching the bound: 809.
///
/// # Arguments
/// - `toml_reader`: an unbuffered `File` in production; in-memory input in tests.
/// - `key_bytes`: the key (non-empty; checked by the public functions).
/// - `scan_goal`: copy the first value, or count key lines.
/// - `output_buffer`: value destination (empty slice for the count goal).
///
/// # Returns
/// `Ok(ScanOutcome)` matching the goal, or `Err(808-814)`.
fn scan_toml_reader_for_key<R: Read>(
    toml_reader: &mut R,
    key_bytes: &[u8],
    scan_goal: ScanGoal,
    output_buffer: &mut [u8],
) -> Result<ScanOutcome, ReadTomlFieldError> {
    let mut scan = ScanContext {
        key_bytes,
        scan_goal,
        output_buffer,
        state: LineScanState::AtLineStart,
        write_cursor: 0,
        unquoted_value_has_pending_cr: false,
        key_line_count: 0,
    };
    let mut single_byte_scratch: [u8; 1] = [0u8; 1];
    let mut bytes_scanned: u64 = 0;
    let maximum_read_calls = RSLSF_MAX_BYTES_SCANNED
        .saturating_add(2)
        .saturating_add(RSLSF_EXTRA_READ_CALLS);

    for _read_call_index in 0..maximum_read_calls {
        let bytes_read = match toml_reader.read(&mut single_byte_scratch) {
            Ok(count) => count,
            Err(io_error) if io_error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_io_error) => {
                #[cfg(debug_assertions)]
                eprintln!(
                    "RSLSF-{}: read failed: {}",
                    ReadTomlFieldError::RsLsfReadFailed.code(),
                    _io_error
                );
                return Err(ReadTomlFieldError::RsLsfReadFailed);
            }
        };
        if bytes_read == 0 {
            return finish_scan_at_end_of_input(&scan);
        }

        bytes_scanned = bytes_scanned.saturating_add(1);
        if bytes_scanned > RSLSF_MAX_BYTES_SCANNED {
            #[cfg(debug_assertions)]
            eprintln!(
                "RSLSF-{}: file exceeds {} bytes",
                ReadTomlFieldError::RsLsfFileExceedsScanLimit.code(),
                RSLSF_MAX_BYTES_SCANNED
            );
            return Err(ReadTomlFieldError::RsLsfFileExceedsScanLimit);
        }

        let [current_byte] = single_byte_scratch;
        if let Some(scan_outcome) = apply_byte_to_scan(&mut scan, current_byte)? {
            return Ok(scan_outcome);
        }
    }

    #[cfg(debug_assertions)]
    eprintln!(
        "RSLSF-{}: read-call limit {} reached",
        ReadTomlFieldError::RsLsfReadCallLimitReached.code(),
        maximum_read_calls
    );
    Err(ReadTomlFieldError::RsLsfReadCallLimitReached)
}

/// Applies one input byte to the state machine.
///
/// # Returns
/// - `Ok(None)`: keep scanning.
/// - `Ok(Some(ValueCopied))`: copy goal finished (value terminated).
/// - `Err(RsLsfValueExceedsOutputBuffer)`: value longer than the buffer.
/// - `Err(RsLsfKeyMatchIndexInvalid)`: internal invariant (see `match_key_byte`).
fn apply_byte_to_scan(
    scan: &mut ScanContext<'_, '_>,
    current_byte: u8,
) -> Result<Option<ScanOutcome>, ReadTomlFieldError> {
    match scan.state {
        LineScanState::AtLineStart | LineScanState::SkippingLeadingWhitespace => match current_byte
        {
            b'\n' => scan.state = LineScanState::AtLineStart,
            // A CR at line start (blank CRLF line) is ignored.
            b'\r' => {}
            b' ' | b'\t' => scan.state = LineScanState::SkippingLeadingWhitespace,
            b'#' => scan.state = LineScanState::InCommentToEndOfLine,
            _ => scan.state = match_key_byte(scan.key_bytes, 0, current_byte)?,
        },

        LineScanState::MatchingKey { matched_key_bytes } => {
            scan.state = match_key_byte(scan.key_bytes, matched_key_bytes, current_byte)?;
        }

        LineScanState::AwaitingEquals => match current_byte {
            b' ' | b'\t' => {}
            b'=' => match scan.scan_goal {
                ScanGoal::CopyFirstValue => scan.state = LineScanState::AwaitingValueStart,
                ScanGoal::CountKeyLines => {
                    scan.key_line_count = scan.key_line_count.saturating_add(1);
                    scan.state = LineScanState::SkippingToEndOfLine;
                }
            },
            // Key matched but the line has no `=`: not an assignment.
            b'\n' => scan.state = LineScanState::AtLineStart,
            // E.g. `name_long` when the key is `name`: not this key.
            _ => scan.state = LineScanState::SkippingToEndOfLine,
        },

        LineScanState::AwaitingValueStart => match current_byte {
            b' ' | b'\t' => {}
            b'"' => scan.state = LineScanState::CopyingQuotedValue,
            // `key =` then newline: empty value (write_cursor is 0 here).
            b'\n' => {
                return Ok(Some(ScanOutcome::ValueCopied {
                    written_length: scan.write_cursor,
                }));
            }
            b'\r' => {
                scan.unquoted_value_has_pending_cr = true;
                scan.state = LineScanState::CopyingUnquotedValue;
            }
            _ => {
                scan.push_value_byte(current_byte)?;
                scan.state = LineScanState::CopyingUnquotedValue;
            }
        },

        LineScanState::CopyingUnquotedValue => match current_byte {
            // Terminator; a pending CR is the CR of a CRLF ending and is dropped.
            b'\n' => {
                return Ok(Some(ScanOutcome::ValueCopied {
                    written_length: scan.write_cursor,
                }));
            }
            b'\r' => {
                // A previous pending CR was not followed by LF: it is value data.
                if scan.unquoted_value_has_pending_cr {
                    scan.push_value_byte(b'\r')?;
                }
                scan.unquoted_value_has_pending_cr = true;
            }
            _ => {
                if scan.unquoted_value_has_pending_cr {
                    scan.push_value_byte(b'\r')?;
                    scan.unquoted_value_has_pending_cr = false;
                }
                scan.push_value_byte(current_byte)?;
            }
        },

        LineScanState::CopyingQuotedValue => {
            if current_byte == b'"' {
                return Ok(Some(ScanOutcome::ValueCopied {
                    written_length: scan.write_cursor,
                }));
            }
            scan.push_value_byte(current_byte)?;
        }

        LineScanState::SkippingToEndOfLine | LineScanState::InCommentToEndOfLine => {
            if current_byte == b'\n' {
                scan.state = LineScanState::AtLineStart;
            }
        }
    }
    Ok(None)
}

/// Compares one byte against the key at position `matched_key_bytes`.
///
/// # Returns
/// - `Ok(AwaitingEquals)`: this byte completed the key.
/// - `Ok(MatchingKey { n + 1 })`: matched, key not complete yet.
/// - `Ok(AtLineStart)`: mismatch on `\n` (short line).
/// - `Ok(SkippingToEndOfLine)`: any other mismatch.
/// - `Err(RsLsfKeyMatchIndexInvalid)`: `matched_key_bytes` is outside the key.
///   Internal invariant: the state machine only stores positions < key length
///   (an empty key is rejected before scanning).
fn match_key_byte(
    key_bytes: &[u8],
    matched_key_bytes: usize,
    current_byte: u8,
) -> Result<LineScanState, ReadTomlFieldError> {
    let expected_key_byte = match key_bytes.get(matched_key_bytes) {
        Some(&key_byte) => key_byte,
        None => {
            #[cfg(all(debug_assertions, not(test)))]
            debug_assert!(
                false,
                "RSLSF-{}: key-match position {} outside key of {} bytes",
                ReadTomlFieldError::RsLsfKeyMatchIndexInvalid.code(),
                matched_key_bytes,
                key_bytes.len()
            );
            #[cfg(debug_assertions)]
            eprintln!(
                "RSLSF-{}: key-match position {} outside key of {} bytes",
                ReadTomlFieldError::RsLsfKeyMatchIndexInvalid.code(),
                matched_key_bytes,
                key_bytes.len()
            );
            return Err(ReadTomlFieldError::RsLsfKeyMatchIndexInvalid);
        }
    };

    if current_byte == expected_key_byte {
        // Cannot overflow: matched_key_bytes < key_bytes.len() was just confirmed.
        let next_matched_key_bytes = matched_key_bytes.saturating_add(1);
        if next_matched_key_bytes == key_bytes.len() {
            Ok(LineScanState::AwaitingEquals)
        } else {
            Ok(LineScanState::MatchingKey {
                matched_key_bytes: next_matched_key_bytes,
            })
        }
    } else if current_byte == b'\n' {
        Ok(LineScanState::AtLineStart)
    } else {
        Ok(LineScanState::SkippingToEndOfLine)
    }
}

/// Decides the result when the input ends.
///
/// # Returns
/// - Count goal: `Ok(KeyLinesCounted)` in any state.
/// - Copy goal, inside a value or right after `=`:
///   `Err(RsLsfValueUnterminatedAtEndOfFile)` (input validation).
/// - Copy goal, any other state: `Err(RsLsfFieldNotFound)` (input validation).
fn finish_scan_at_end_of_input(
    scan: &ScanContext<'_, '_>,
) -> Result<ScanOutcome, ReadTomlFieldError> {
    match scan.scan_goal {
        ScanGoal::CountKeyLines => Ok(ScanOutcome::KeyLinesCounted {
            key_line_count: scan.key_line_count,
        }),
        ScanGoal::CopyFirstValue => match scan.state {
            LineScanState::AwaitingValueStart
            | LineScanState::CopyingUnquotedValue
            | LineScanState::CopyingQuotedValue => {
                #[cfg(debug_assertions)]
                eprintln!(
                    "RSLSF-{}: end of file inside value of key {:?}",
                    ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile.code(),
                    String::from_utf8_lossy(scan.key_bytes)
                );
                Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile)
            }
            LineScanState::AtLineStart
            | LineScanState::SkippingLeadingWhitespace
            | LineScanState::MatchingKey { .. }
            | LineScanState::AwaitingEquals
            | LineScanState::SkippingToEndOfLine
            | LineScanState::InCommentToEndOfLine => {
                #[cfg(debug_assertions)]
                eprintln!(
                    "RSLSF-{}: key {:?} not found",
                    ReadTomlFieldError::RsLsfFieldNotFound.code(),
                    String::from_utf8_lossy(scan.key_bytes)
                );
                Err(ReadTomlFieldError::RsLsfFieldNotFound)
            }
        },
    }
}

/// Extracts the value length from a copy-goal outcome.
/// `Err(RsLsfScanOutcomeMismatch)`: internal invariant (a count result
/// returned for a copy scan).
fn copied_value_length_from_outcome(
    scan_outcome: ScanOutcome,
) -> Result<usize, ReadTomlFieldError> {
    match scan_outcome {
        ScanOutcome::ValueCopied { written_length } => Ok(written_length),
        ScanOutcome::KeyLinesCounted { .. } => {
            #[cfg(all(debug_assertions, not(test)))]
            debug_assert!(
                false,
                "RSLSF-{}: copy scan returned a count",
                ReadTomlFieldError::RsLsfScanOutcomeMismatch.code()
            );
            #[cfg(debug_assertions)]
            eprintln!(
                "RSLSF-{}: copy scan returned a count",
                ReadTomlFieldError::RsLsfScanOutcomeMismatch.code()
            );
            Err(ReadTomlFieldError::RsLsfScanOutcomeMismatch)
        }
    }
}

/// Extracts the key-line count from a count-goal outcome.
/// `Err(RsLsfScanOutcomeMismatch)`: internal invariant (a copied value
/// returned for a count scan).
fn key_line_count_from_outcome(scan_outcome: ScanOutcome) -> Result<u32, ReadTomlFieldError> {
    match scan_outcome {
        ScanOutcome::KeyLinesCounted { key_line_count } => Ok(key_line_count),
        ScanOutcome::ValueCopied { .. } => {
            #[cfg(all(debug_assertions, not(test)))]
            debug_assert!(
                false,
                "RSLSF-{}: count scan returned a value",
                ReadTomlFieldError::RsLsfScanOutcomeMismatch.code()
            );
            #[cfg(debug_assertions)]
            eprintln!(
                "RSLSF-{}: count scan returned a value",
                ReadTomlFieldError::RsLsfScanOutcomeMismatch.code()
            );
            Err(ReadTomlFieldError::RsLsfScanOutcomeMismatch)
        }
    }
}

// ============================================================================
// Tests (test mode only)
// ============================================================================

/*
Default test.toml

```toml
text = "production"
longtext = "productionproductionproductionproductionproductionproductionproduction"
```
*/

/// Unit tests (compiled only under `cargo test`).
///
/// Scanner tests run `scan_toml_reader_for_key` on in-memory input
/// (`Cursor`, `io::repeat`, and small failing readers), so they create no
/// files. Tests of the public functions create uniquely named files under
/// `std::env::temp_dir()` and remove them at the end; if such a test fails
/// mid-way its file may remain (harmless, unique name).
///
/// Test names: `rslsf_<area>_<condition>[_with_code_<nnn>]`.
/// Areas: `scan`, `count`, `read`, `unique`, `err`, `guard`.
/// `unwrap`/`assert!`/indexing are allowed here (test mode).
#[cfg(test)]
mod read_toml_single_line_string_field_no_heap_tests {
    use super::*;
    use std::io::Cursor;
    use std::path::PathBuf;

    /// Runs a copy-goal scan on `toml_text` with a `buffer_length`-byte
    /// output buffer; returns the copied value bytes.
    fn copy_first_value(
        toml_text: &str,
        key: &str,
        buffer_length: usize,
    ) -> Result<Vec<u8>, ReadTomlFieldError> {
        let mut output_buffer = vec![0u8; buffer_length];
        let mut toml_reader = Cursor::new(toml_text.as_bytes());
        match scan_toml_reader_for_key(
            &mut toml_reader,
            key.as_bytes(),
            ScanGoal::CopyFirstValue,
            &mut output_buffer,
        )? {
            ScanOutcome::ValueCopied { written_length } => {
                Ok(output_buffer[..written_length].to_vec())
            }
            ScanOutcome::KeyLinesCounted { .. } => panic!("copy scan returned a count"),
        }
    }

    /// Runs a count-goal scan on `toml_text`; returns the key-line count.
    fn count_key_lines(toml_text: &str, key: &str) -> Result<u32, ReadTomlFieldError> {
        let mut toml_reader = Cursor::new(toml_text.as_bytes());
        let mut no_value_buffer: [u8; 0] = [];
        match scan_toml_reader_for_key(
            &mut toml_reader,
            key.as_bytes(),
            ScanGoal::CountKeyLines,
            &mut no_value_buffer,
        )? {
            ScanOutcome::KeyLinesCounted { key_line_count } => Ok(key_line_count),
            ScanOutcome::ValueCopied { .. } => panic!("count scan returned a value"),
        }
    }

    /// Writes `contents` to a uniquely named file under the temp dir and
    /// returns its absolute path. The caller removes the file.
    fn write_unique_temp_toml(label: &str, contents: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rslsf_test_{}_{}_{}.toml",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Reader that reports "interrupted" before every real read.
    struct InterruptEveryOtherRead {
        inner: Cursor<Vec<u8>>,
        interrupt_next: bool,
    }

    impl Read for InterruptEveryOtherRead {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.interrupt_next {
                self.interrupt_next = false;
                return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
            }
            self.interrupt_next = true;
            self.inner.read(buffer)
        }
    }

    /// Reader that reports "interrupted" forever.
    struct AlwaysInterruptedReader;

    impl Read for AlwaysInterruptedReader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
        }
    }

    /// Reader whose every read fails with a non-retryable error.
    struct AlwaysFailingReader;

    impl Read for AlwaysFailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::Other))
        }
    }

    /// Basic case: a quoted value is copied without its quotes.
    #[test]
    fn rslsf_scan_copies_quoted_value() {
        assert_eq!(
            copy_first_value("name = \"alice\"\n", "name", 16).unwrap(),
            b"alice"
        );
    }

    /// Unquoted values (numbers, bare words) end at the newline.
    #[test]
    fn rslsf_scan_copies_unquoted_value() {
        assert_eq!(
            copy_first_value("port = 8080\n", "port", 16).unwrap(),
            b"8080"
        );
    }

    /// CRLF files: the quoted value is unaffected by the CR.
    #[test]
    fn rslsf_scan_handles_crlf_for_quoted_value() {
        let value = copy_first_value("name = \"bob\"\r\nother = \"x\"\r\n", "name", 16).unwrap();
        assert_eq!(value, b"bob");
    }

    /// CRLF files: the CR before LF is not part of an unquoted value.
    #[test]
    fn rslsf_scan_handles_crlf_for_unquoted_value() {
        assert_eq!(
            copy_first_value("port = 8080\r\nx = 1\r\n", "port", 16).unwrap(),
            b"8080"
        );
    }

    /// Comment lines (also indented) and blank lines are skipped.
    #[test]
    fn rslsf_scan_skips_comments_and_blank_lines() {
        let toml_text = "# header\n\n   # indented comment\nname = \"carol\"\n";
        assert_eq!(copy_first_value(toml_text, "name", 16).unwrap(), b"carol");
    }

    /// A longer key that starts with the requested key is not a match.
    #[test]
    fn rslsf_scan_does_not_match_longer_key_with_same_prefix() {
        let toml_text = "name_long = \"WRONG\"\nname = \"RIGHT\"\n";
        assert_eq!(copy_first_value(toml_text, "name", 16).unwrap(), b"RIGHT");
    }

    /// A key that no line assigns is reported, not defaulted.
    #[test]
    fn rslsf_scan_missing_key_with_code_811() {
        let error = copy_first_value("other = \"x\"\n", "name", 16).unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfFieldNotFound);
        assert_eq!(error.code(), 811);
    }

    /// A value longer than the buffer (9 bytes into 8) is refused, never truncated.
    #[test]
    fn rslsf_scan_value_longer_than_buffer_with_code_812() {
        let error = copy_first_value("name = \"toolongxx\"\n", "name", 8).unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
        assert_eq!(error.code(), 812);
    }

    /// A quoted value needs no trailing newline: the closing quote ends it.
    #[test]
    fn rslsf_scan_quoted_value_at_end_of_file_is_complete() {
        assert_eq!(
            copy_first_value("name = \"dora\"", "name", 16).unwrap(),
            b"dora"
        );
    }

    /// An unquoted value without a trailing newline is incomplete (policy:
    /// unquoted values need `\n`, as quoted values need `"`).
    #[test]
    fn rslsf_scan_unquoted_value_without_newline_with_code_813() {
        let error = copy_first_value("port = 8080", "port", 16).unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile);
        assert_eq!(error.code(), 813);
    }

    /// A quoted value without a closing quote reads through the newline to
    /// end of file and is reported as incomplete.
    #[test]
    fn rslsf_scan_quoted_value_without_closing_quote_with_code_813() {
        let error = copy_first_value("name = \"alice\n", "name", 16).unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile);
    }

    /// The key is found after many unrelated lines.
    #[test]
    fn rslsf_scan_finds_key_after_many_lines() {
        let mut toml_text = String::new();
        for line_index in 0..200 {
            toml_text.push_str(&format!("noise_key_{:03} = \"junkjunkjunk\"\n", line_index));
        }
        toml_text.push_str("target = \"eve\"\n");
        assert_eq!(copy_first_value(&toml_text, "target", 16).unwrap(), b"eve");
    }

    /// An unrelated line far longer than the output buffer does not stop the
    /// scan: only the matched value is stored.
    #[test]
    fn rslsf_scan_unrelated_long_line_does_not_stop_scan() {
        let toml_text = format!("other_key = \"{}\"\nname = \"frank\"\n", "X".repeat(4096));
        assert_eq!(copy_first_value(&toml_text, "name", 16).unwrap(), b"frank");
    }

    /// Spaces and tabs are allowed before the key and around `=`.
    #[test]
    fn rslsf_scan_allows_whitespace_around_key_and_equals() {
        let value = copy_first_value("   name\t=\t  \"grace\"   \n", "name", 16).unwrap();
        assert_eq!(value, b"grace");
    }

    /// The read returns the first assignment (duplicates are the optional
    /// uniqueness check's job).
    #[test]
    fn rslsf_scan_first_assignment_wins() {
        let value = copy_first_value("name = \"first\"\nname = \"second\"\n", "name", 16).unwrap();
        assert_eq!(value, b"first");
    }

    /// Policy: spaces before the newline are part of an unquoted value.
    #[test]
    fn rslsf_scan_unquoted_value_keeps_trailing_whitespace() {
        assert_eq!(
            copy_first_value("port = 8080   \n", "port", 16).unwrap(),
            b"8080   "
        );
    }

    /// `key =` then newline is an empty value, not an error.
    #[test]
    fn rslsf_scan_empty_unquoted_value_is_empty() {
        assert!(
            copy_first_value("name = \n", "name", 16)
                .unwrap()
                .is_empty()
        );
    }

    /// `key = ""` is an empty value.
    #[test]
    fn rslsf_scan_empty_quoted_value_is_empty() {
        assert!(
            copy_first_value("name = \"\"\n", "name", 16)
                .unwrap()
                .is_empty()
        );
    }

    /// A one-byte key completes on its first byte (general match path).
    #[test]
    fn rslsf_scan_single_byte_key() {
        assert_eq!(copy_first_value("x = \"yes\"\n", "x", 16).unwrap(), b"yes");
    }

    /// Interrupted reads are retried, and the result is unchanged.
    #[test]
    fn rslsf_scan_retries_interrupted_reads() {
        let mut toml_reader = InterruptEveryOtherRead {
            inner: Cursor::new(b"name = \"ivy\"\n".to_vec()),
            interrupt_next: true,
        };
        let mut output_buffer = [0u8; 16];
        let outcome = scan_toml_reader_for_key(
            &mut toml_reader,
            b"name",
            ScanGoal::CopyFirstValue,
            &mut output_buffer,
        )
        .unwrap();
        assert_eq!(outcome, ScanOutcome::ValueCopied { written_length: 3 });
        assert_eq!(&output_buffer[..3], b"ivy");
    }

    /// Endless interruptions end at the read-call bound instead of looping forever.
    #[test]
    fn rslsf_scan_endless_interruptions_with_code_809() {
        let mut output_buffer = [0u8; 16];
        let error = scan_toml_reader_for_key(
            &mut AlwaysInterruptedReader,
            b"name",
            ScanGoal::CopyFirstValue,
            &mut output_buffer,
        )
        .unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfReadCallLimitReached);
        assert_eq!(error.code(), 809);
    }

    /// A failing read stops the scan with its own code.
    #[test]
    fn rslsf_scan_read_failure_with_code_808() {
        let mut output_buffer = [0u8; 16];
        let error = scan_toml_reader_for_key(
            &mut AlwaysFailingReader,
            b"name",
            ScanGoal::CopyFirstValue,
            &mut output_buffer,
        )
        .unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfReadFailed);
        assert_eq!(error.code(), 808);
    }

    /// Input larger than the scan limit (here: endless) stops at the byte cap.
    #[test]
    fn rslsf_scan_input_exceeds_limit_with_code_810() {
        let mut endless_input = std::io::repeat(b'x');
        let mut output_buffer = [0u8; 16];
        let error = scan_toml_reader_for_key(
            &mut endless_input,
            b"name",
            ScanGoal::CopyFirstValue,
            &mut output_buffer,
        )
        .unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfFileExceedsScanLimit);
        assert_eq!(error.code(), 810);
    }

    /// Key-line counting: assignments are counted; comments, longer keys,
    /// and lines without `=` are not; indented assignments are.
    #[test]
    fn rslsf_count_counts_only_assignment_lines() {
        let toml_text = "name = \"a\"\n# name = \"b\"\nname_long = \"c\"\nname\n  name=\"d\"\n";
        assert_eq!(count_key_lines(toml_text, "name").unwrap(), 2);
        assert_eq!(count_key_lines(toml_text, "absent").unwrap(), 0);
    }

    /// Invariant catch: a key-match position outside the key (state built
    /// directly, simulating corruption) is rejected, not indexed.
    #[test]
    fn rslsf_scan_corrupt_match_position_with_code_814() {
        let mut no_value_buffer: [u8; 0] = [];
        let mut scan = ScanContext {
            key_bytes: b"name",
            scan_goal: ScanGoal::CopyFirstValue,
            output_buffer: &mut no_value_buffer,
            state: LineScanState::MatchingKey {
                matched_key_bytes: 99,
            },
            write_cursor: 0,
            unquoted_value_has_pending_cr: false,
            key_line_count: 0,
        };
        let error = apply_byte_to_scan(&mut scan, b'a').unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfKeyMatchIndexInvalid);
        assert_eq!(error.code(), 814);
    }

    /// Invariant catch: each goal's extractor rejects the other goal's result.
    #[test]
    fn rslsf_scan_outcome_mismatch_with_code_815() {
        let error =
            copied_value_length_from_outcome(ScanOutcome::KeyLinesCounted { key_line_count: 1 })
                .unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfScanOutcomeMismatch);
        assert_eq!(error.code(), 815);
        assert_eq!(
            key_line_count_from_outcome(ScanOutcome::ValueCopied { written_length: 0 })
                .unwrap_err(),
            ReadTomlFieldError::RsLsfScanOutcomeMismatch
        );
    }

    /// End to end on a real file: value read; later buffer bytes are 0.
    #[test]
    fn rslsf_read_reads_value_from_file() {
        let path = write_unique_temp_toml("read_ok", "mode = \"production\"\nport = 8080\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(&path, "mode").unwrap();
        assert_eq!(&output_buffer[..written_length], b"production");
        assert!(
            output_buffer[written_length..]
                .iter()
                .all(|byte| *byte == 0)
        );
        std::fs::remove_file(&path).unwrap();
    }

    /// A relative path is refused before any file access.
    #[test]
    fn rslsf_read_relative_path_with_code_803() {
        let error = read_single_line_string_field_from_toml_no_heap::<16>(
            Path::new("relative/config.toml"),
            "name",
        )
        .unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfPathNotAbsolute);
        assert_eq!(error.code(), 803);
    }

    /// A missing file fails fast (not retryable, no backoff sleeps).
    #[test]
    fn rslsf_read_missing_file_with_code_804() {
        let error = read_single_line_string_field_from_toml_no_heap::<16>(
            Path::new("/nonexistent_rslsf_test_dir/missing.toml"),
            "name",
        )
        .unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfOpenNotFound);
        assert_eq!(error.code(), 804);
        assert!(!error.is_retryable());
    }

    /// An empty key is refused before the path is checked or opened.
    #[test]
    fn rslsf_read_empty_key_with_code_801() {
        let error = read_single_line_string_field_from_toml_no_heap::<16>(
            Path::new("/nonexistent_rslsf_test_dir/any.toml"),
            "",
        )
        .unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfEmptyKey);
        assert_eq!(error.code(), 801);
    }

    /// A zero-size output buffer is refused before any file access.
    #[test]
    fn rslsf_read_zero_size_buffer_with_code_802() {
        let error = read_single_line_string_field_from_toml_no_heap::<0>(
            Path::new("/nonexistent_rslsf_test_dir/any.toml"),
            "name",
        )
        .unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfOutputBufferZeroSized);
        assert_eq!(error.code(), 802);
    }

    /// Uniqueness check on a real file: unique keys pass, a key assigned
    /// twice returns 816, and the per-key count shows which key.
    #[test]
    fn rslsf_unique_duplicate_key_with_code_816() {
        let path = write_unique_temp_toml("unique", "mode = \"a\"\nport = 1\nmode = \"b\"\n");
        assert!(check_toml_keys_are_unique_no_heap(&path, &["port"]).is_ok());
        let error = check_toml_keys_are_unique_no_heap(&path, &["port", "mode"]).unwrap_err();
        assert_eq!(error, ReadTomlFieldError::RsLsfDuplicateKey);
        assert_eq!(error.code(), 816);
        assert_eq!(count_toml_key_lines_no_heap(&path, "mode").unwrap(), 2);
        std::fs::remove_file(&path).unwrap();
    }

    /// Retry policy is per code: only the transient open code retries.
    #[test]
    fn rslsf_err_is_retryable_only_transient_open() {
        assert!(ReadTomlFieldError::RsLsfOpenTransient.is_retryable());
        assert!(!ReadTomlFieldError::RsLsfOpenNotFound.is_retryable());
        assert!(!ReadTomlFieldError::RsLsfReadCallLimitReached.is_retryable());
    }

    /// Single source of truth for code numbers: no message may contain the
    /// marker followed by a digit; numbers come only from `.code()`.
    #[test]
    fn rslsf_guard_source_contains_no_hard_coded_error_code_numbers() {
        let source_files: [(&str, &str); 2] = [
            (
                "read_toml_single_line_string_field_no_heap.rs",
                include_str!("read_toml_single_line_string_field_no_heap.rs"),
            ),
            ("main.rs", include_str!("main.rs")),
        ];
        let marker = "RSLSF-";
        for (file_name, source_text) in source_files.iter() {
            for (line_index, line_text) in source_text.lines().enumerate() {
                for (found_index, _) in line_text.match_indices(marker) {
                    let byte_after_marker = line_text.as_bytes().get(found_index + marker.len());
                    assert!(
                        !matches!(byte_after_marker, Some(byte) if byte.is_ascii_digit()),
                        "{}:{}: hard-coded error-code number: {}",
                        file_name,
                        line_index + 1,
                        line_text.trim()
                    );
                }
            }
        }
    }
}

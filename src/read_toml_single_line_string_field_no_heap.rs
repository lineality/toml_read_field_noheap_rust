//! ============================================================================
//! Module: read_toml_single_line_string_field_no_heap
//! ============================================================================
//!
//! https://github.com/lineality/toml_read_field_noheap_rust
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
//! If the scope of the task is to read a single small value
//! contingent on a small key in a .toml file line,
//! then that process to do that should:
//! - not use heap memory at all
//! - not read the entire file into memory
//! - not read the entire line into memory
//! - not read any large chunk of key + value into memory
//! - not read the key more than one byte at a time
//! - not use more than a one multi-byte buffer sanitized to be no larger
//!   than a the value: two buffers, one single-byte buffer
//!   and one multi-byte buffer (e.g. sized to fit the value).
//!
//! For security and efficiency no more than what is needed should be
//! loaded or read, and what initial "reading" only requires a single byte
//! (a one-byte buffer).
//!
//! This module exposes a single function,
//! [`read_single_line_string_field_from_toml_no_heap`], that reads a single
//! short-string field from a TOML file using only stack-allocated buffers.
//!
//! # Memory Sanitation
//!
//! Bounding a buffer to the smallest size the task actually requires is
//! a form of input sanitation: input that exceeds what the task is meant
//! to handle cannot enter, because there is nowhere to put it. The
//! const-generic `OUTPUT_BUFFER_BYTES` and the `RsLsfValueExceedsOutputBuffer`
//! error together make oversize values unrepresentable rather than
//! silently absorbed. Buffers that read in more bytes than the task
//! requires (e.g. `BufReader`'s ~8 KiB default, or "read the whole line
//! and sort it out later") are a sanitation failure in the same family
//! as unbounded reads and over-sized allocations — Heartbleed being a
//! well-known example of the broader class. This module bounds every
//! buffer to the smallest size the task genuinely needs: one byte for
//! reading from the file, and exactly `OUTPUT_BUFFER_BYTES` for the
//! value being returned.
//!
//! This is in the same spirit as the combined security and efficiency
//! of using enums and structs in Rust to require inputs to be very
//! strictly only what they are safe to be: hygiene and sanitation for
//! economics, efficiency, maintainability, modularity, and security.
//!
//! # Architecture
//!
//! The scanner reads **one byte at a time** directly from the file and walks
//! it through a finite-state machine. There is NO read-chunk buffer and NO
//! line-accumulator buffer. The only buffer in the function is the caller's
//! output buffer, into which value bytes are written directly when (and only
//! when) the scanner is inside the value of the matched key.
//!
//! State carried by the scanner:
//!
//!   * one `[u8; 1]` read scratch (typically a CPU register, not even stack)
//!   * one small state enum + one `usize` (`matched_key_bytes`)
//!   * one `usize` write cursor into the caller's output buffer
//!   * two `u64` failsafe counters (bytes scanned, iteration count)
//!
//! Total module-internal scratch: a few words. The key itself is never copied
//! anywhere (the key is not read into memory or a buffer, it is 'scanned'
//! one byte at a time),
//! it is compared against `target_field_key.as_bytes()` in place,
//! index by index.
//!
//! # In Scope
//!
//! * One key per call, top-level (no `[section]`).
//! * Single-line values up to a caller-chosen `OUTPUT_BUFFER_BYTES` length.
//! * Values quoted with simple double quotes (`"..."`) or unquoted (numbers,
//!   bare identifiers).
//! * Lines using LF or CRLF terminators.
//! * Lines beginning with `#` (after trimming leading whitespace) are treated
//!   as comments.
//!
//! # Value Termination Policy
//!
//! * **Quoted values** terminate at the next `"` byte. EOF reached before the
//!   closing `"` is an error (`RsLsfValueUnterminatedAtEndOfFile`).
//! * **Unquoted values** terminate at the next `\n` byte (a preceding `\r`,
//!   if any, is not included in the value). EOF reached before `\n` is an
//!   error (`RsLsfValueUnterminatedAtEndOfFile`). This symmetry — quoted
//!   needs a closing quote, unquoted needs a closing newline — was a
//!   deliberate design choice. Trailing whitespace between the last
//!   non-whitespace value byte and `\n` IS included in the returned value
//!   (strict policy: zero extra state, caller trims if desired).
//!
//! # Explicitly Out Of Scope (Non-Goals)
//!
//! * Full TOML grammar (no arrays, tables, inline tables, multi-line strings,
//!   escape sequences, dotted keys, datetimes).
//! * Re-encoding the value (caller decides whether to `core::str::from_utf8`).
//! * Trailing inline comments on the same line as the value
//!   (e.g. `name = "x"  # note` — the `# note` becomes part of the value for
//!   unquoted values; for quoted values it is ignored because termination
//!   occurs at the closing `"`).
//! * UTF-8 BOM at file start.
//!
//! # Defensive Policy
//!
//! On any malformed input, I/O error, oversize value, or exhausted safety
//! budget, the function returns a terse zero-data [`ReadTomlFieldError`]
//! variant. It never panics, never allocates, and never includes the file
//! path, file contents, or OS error string in the returned error.
//!
//! # Concurrency
//!
//! The function is synchronous and self-contained. It does not share state.
//! It is safe to call from multiple threads with distinct paths.
//! ============================================================================

use std::fs::File;
use std::io::Read;

// ----------------------------------------------------------------------------
// Module constants
// ----------------------------------------------------------------------------

/// Failsafe upper bound on total bytes read from a single file.
///
/// Bounds the read loop even if the OS keeps returning data (NASA P10 rule 2).
/// One mebibyte is generous for configuration files while preventing
/// pathological or adversarial inputs from running unbounded work.
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
///   no embedded OS error. Error values must never become an
///   information-disclosure vector.
/// * Every variant carries the unique prefix `RsLsf` (Read Single Line
///   String Field) so it is unambiguously traceable in logs to this
///   function, satisfying the "unique error per function" rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadTomlFieldError {
    /// RSLSF: caller-supplied key was empty.
    RsLsfEmptyKey,
    /// RSLSF: caller-supplied `OUTPUT_BUFFER_BYTES` const-generic was zero.
    RsLsfOutputBufferZeroSized,
    /// RSLSF: the file could not be opened (does not exist, permission, etc.).
    RsLsfFileOpenFailed,
    /// RSLSF: an I/O error occurred while reading.
    RsLsfFileReadFailed,
    /// RSLSF: the requested key was not present in the file.
    RsLsfFieldNotFound,
    /// RSLSF: a value would not fit in `OUTPUT_BUFFER_BYTES`; refusing to
    /// silently truncate.
    RsLsfValueExceedsOutputBuffer,
    /// RSLSF: end-of-file was reached while still inside a value — no
    /// closing `"` for a quoted value, or no terminating `\n` for an
    /// unquoted value. Refusing to guess a terminator.
    RsLsfValueUnterminatedAtEndOfFile,
    /// RSLSF: the failsafe byte/iteration budget was exhausted.
    RsLsfSafetyBudgetExhausted,
}

// ----------------------------------------------------------------------------
// Internal scanner state
// ----------------------------------------------------------------------------

/// Finite-state machine for the byte-at-a-time scanner.
///
/// The scanner is fed one input byte per outer-loop iteration. The current
/// state plus that byte determines the next state (and possibly a write into
/// the output buffer or an early return).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineScanState {
    /// Just after `\n` (or at file start). Whitespace, `#`, `\n`, or the
    /// first byte of a candidate key may appear next.
    AtLineStart,
    /// Saw leading whitespace after line start; same transitions as
    /// `AtLineStart` except a `#` here is still a full-line comment.
    SkippingLeadingWhitespace,
    /// Comparing input bytes against `target_field_key`. `matched_key_bytes`
    /// counts how many bytes have matched so far.
    MatchingKey { matched_key_bytes: usize },
    /// Full key has matched. Expecting optional whitespace then `=`.
    AwaitingEquals,
    /// Saw `=`. Skipping optional whitespace before the value begins.
    AwaitingValueStart,
    /// Inside an unquoted value. Terminates on `\n`. `\r` immediately
    /// before `\n` is dropped, not stored.
    CopyingUnquotedValue,
    /// Inside a quoted value. Terminates on `"`.
    CopyingQuotedValue,
    /// This line cannot match the key; fast-forward to the next `\n`.
    SkippingToEndOfLine,
    /// Saw `#` at the start of a line; treat the rest of the line as comment.
    InCommentToEndOfLine,
}

// ----------------------------------------------------------------------------
// Public API
// ----------------------------------------------------------------------------

/// Reads a single top-level single-line string field from a TOML file using
/// only stack-allocated memory.
///
/// One byte is read from the file at a time. There is no read-chunk buffer
/// and no line-accumulator buffer. Value bytes (and only value bytes for the
/// matched key) are written directly into the caller's output buffer.
///
/// # Type Parameters
/// * `OUTPUT_BUFFER_BYTES` — the fixed size of the returned byte buffer.
///   Must be `> 0`. Pick the smallest value that comfortably fits your
///   project's value (e.g. `16` for a short identifier). A value longer
///   than `OUTPUT_BUFFER_BYTES` produces `RsLsfValueExceedsOutputBuffer`;
///   values are never silently truncated.
///
/// # Arguments
/// * `absolute_toml_file_path` — absolute path to the TOML file.
/// * `target_field_key` — the exact top-level key to find. Must be non-empty.
///
/// # Returns
/// * `Ok((output_buffer, written_length))` on success. `written_length`
///   bytes of `output_buffer` are meaningful; bytes past that are zero.
/// * `Err(ReadTomlFieldError)` on any failure; never panics, never allocates.
///
/// # Example (illustrative)
/// ```ignore
/// match read_single_line_string_field_from_toml_no_heap::<16>(
///     "/etc/myapp/config.toml",
///     "node_id",
/// ) {
///     Ok((buf, len)) => {
///         if let Ok(s) = core::str::from_utf8(&buf[..len]) {
///             // use s
///         }
///     }
///     Err(_e) => {
///         // Log a unique short code; do NOT log path/contents.
///         // Continue with a safe default; do not panic.
///     }
/// }
/// ```
pub fn read_single_line_string_field_from_toml_no_heap<const OUTPUT_BUFFER_BYTES: usize>(
    absolute_toml_file_path: &str,
    target_field_key: &str,
) -> Result<([u8; OUTPUT_BUFFER_BYTES], usize), ReadTomlFieldError> {
    // ----------------------------------------------------------------------
    // Debug-Assert / Test-Assert / Production-Catch
    // ----------------------------------------------------------------------
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

    if OUTPUT_BUFFER_BYTES == 0 {
        return Err(ReadTomlFieldError::RsLsfOutputBufferZeroSized);
    }
    if target_field_key.is_empty() {
        return Err(ReadTomlFieldError::RsLsfEmptyKey);
    }

    // ----------------------------------------------------------------------
    // Open the file (terse error: no path leakage)
    // ----------------------------------------------------------------------
    let mut open_file_handle: File = match File::open(absolute_toml_file_path) {
        Ok(handle) => handle,
        Err(_) => return Err(ReadTomlFieldError::RsLsfFileOpenFailed),
    };

    // ----------------------------------------------------------------------
    // Scanner state
    // ----------------------------------------------------------------------
    let key_bytes: &[u8] = target_field_key.as_bytes();

    let mut single_byte_read_scratch: [u8; 1] = [0u8; 1];
    let mut output_buffer: [u8; OUTPUT_BUFFER_BYTES] = [0u8; OUTPUT_BUFFER_BYTES];
    let mut output_write_cursor: usize = 0;

    let mut current_state: LineScanState = LineScanState::AtLineStart;

    // For unquoted values: when we see '\r' we hold it pending the next
    // byte. If that next byte is '\n', the '\r' is silently dropped (CRLF).
    // If it is anything else, the held '\r' becomes part of the value.
    // This avoids needing a buffer to "peek".
    let mut unquoted_value_has_pending_cr: bool = false;

    let mut cumulative_bytes_scanned: u64 = 0;
    let mut safety_iteration_count: u64 = 0;
    // One byte per iteration, so the iteration cap matches the byte cap
    // with a small margin to absorb the EOF iteration.
    let safety_iteration_limit: u64 = RSLSF_MAX_BYTES_SCANNED + 16;

    // ----------------------------------------------------------------------
    // Read loop: one byte per iteration.
    // ----------------------------------------------------------------------
    loop {
        safety_iteration_count = safety_iteration_count.saturating_add(1);
        if safety_iteration_count > safety_iteration_limit {
            return Err(ReadTomlFieldError::RsLsfSafetyBudgetExhausted);
        }

        let bytes_read_this_call: usize = match open_file_handle.read(&mut single_byte_read_scratch)
        {
            Ok(count) => count,
            Err(_) => return Err(ReadTomlFieldError::RsLsfFileReadFailed),
        };

        // ------------------------------------------------------------------
        // EOF handling
        // ------------------------------------------------------------------
        if bytes_read_this_call == 0 {
            // EOF. What we do depends on what state we are in.
            match current_state {
                LineScanState::CopyingUnquotedValue => {
                    // Per the value-termination policy, an unquoted value
                    // MUST end with '\n'. EOF mid-value is an error.
                    return Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile);
                }
                LineScanState::CopyingQuotedValue => {
                    // Quoted value never saw its closing '"'.
                    return Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile);
                }
                LineScanState::AwaitingValueStart => {
                    // We saw "key =" then EOF with no value bytes.
                    // For unquoted values policy this also requires '\n'.
                    return Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile);
                }
                _ => {
                    // Any other state means we never entered the value.
                    return Err(ReadTomlFieldError::RsLsfFieldNotFound);
                }
            }
        }

        cumulative_bytes_scanned = cumulative_bytes_scanned.saturating_add(1);
        if cumulative_bytes_scanned > RSLSF_MAX_BYTES_SCANNED {
            return Err(ReadTomlFieldError::RsLsfSafetyBudgetExhausted);
        }

        let current_byte: u8 = single_byte_read_scratch[0];

        // ------------------------------------------------------------------
        // State transition
        // ------------------------------------------------------------------
        match current_state {
            LineScanState::AtLineStart | LineScanState::SkippingLeadingWhitespace => {
                if current_byte == b'\n' {
                    current_state = LineScanState::AtLineStart;
                } else if current_byte == b'\r' {
                    // Drop bare CR (and CR before LF) at line start.
                } else if is_ascii_space_or_tab_byte(current_byte) {
                    current_state = LineScanState::SkippingLeadingWhitespace;
                } else if current_byte == b'#' {
                    current_state = LineScanState::InCommentToEndOfLine;
                } else {
                    // First byte of a candidate key. Compare to key[0].
                    if current_byte == key_bytes[0] {
                        if key_bytes.len() == 1 {
                            // Single-byte key already fully matched.
                            current_state = LineScanState::AwaitingEquals;
                        } else {
                            current_state = LineScanState::MatchingKey {
                                matched_key_bytes: 1,
                            };
                        }
                    } else {
                        current_state = LineScanState::SkippingToEndOfLine;
                    }
                }
            }

            LineScanState::MatchingKey { matched_key_bytes } => {
                if matched_key_bytes < key_bytes.len() {
                    if current_byte == key_bytes[matched_key_bytes] {
                        let next_matched = matched_key_bytes + 1;
                        if next_matched == key_bytes.len() {
                            current_state = LineScanState::AwaitingEquals;
                        } else {
                            current_state = LineScanState::MatchingKey {
                                matched_key_bytes: next_matched,
                            };
                        }
                    } else if current_byte == b'\n' {
                        // Short line; cannot match. Start over.
                        current_state = LineScanState::AtLineStart;
                    } else {
                        current_state = LineScanState::SkippingToEndOfLine;
                    }
                } else {
                    // Defensive: should not be reachable because we
                    // transition to AwaitingEquals as soon as the full key
                    // matches. Treat as non-match for safety.
                    current_state = LineScanState::SkippingToEndOfLine;
                }
            }

            LineScanState::AwaitingEquals => {
                if is_ascii_space_or_tab_byte(current_byte) {
                    // stay
                } else if current_byte == b'=' {
                    current_state = LineScanState::AwaitingValueStart;
                } else if current_byte == b'\n' {
                    // Key matched but no '=' on the line; not a kv pair.
                    current_state = LineScanState::AtLineStart;
                } else {
                    // E.g. "name_long" when looking for "name": extra
                    // characters after key bytes. Not a match.
                    current_state = LineScanState::SkippingToEndOfLine;
                }
            }

            LineScanState::AwaitingValueStart => {
                if is_ascii_space_or_tab_byte(current_byte) {
                    // stay
                } else if current_byte == b'"' {
                    current_state = LineScanState::CopyingQuotedValue;
                } else if current_byte == b'\n' {
                    // Empty unquoted value with a newline terminator: OK,
                    // return zero-length value.
                    return Ok((output_buffer, 0));
                } else if current_byte == b'\r' {
                    // Hold pending CR; next byte decides CRLF vs literal.
                    unquoted_value_has_pending_cr = true;
                    current_state = LineScanState::CopyingUnquotedValue;
                } else {
                    // First byte of an unquoted value.
                    if output_write_cursor >= OUTPUT_BUFFER_BYTES {
                        return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
                    }
                    output_buffer[output_write_cursor] = current_byte;
                    output_write_cursor += 1;
                    current_state = LineScanState::CopyingUnquotedValue;
                }
            }

            LineScanState::CopyingUnquotedValue => {
                if current_byte == b'\n' {
                    // Terminator. Pending CR (if any) is dropped: CRLF.
                    return Ok((output_buffer, output_write_cursor));
                } else if current_byte == b'\r' {
                    // If a previous CR was pending, it was a literal CR
                    // in the value and must be written now.
                    if unquoted_value_has_pending_cr {
                        if output_write_cursor >= OUTPUT_BUFFER_BYTES {
                            return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
                        }
                        output_buffer[output_write_cursor] = b'\r';
                        output_write_cursor += 1;
                    }
                    unquoted_value_has_pending_cr = true;
                } else {
                    // Flush any pending CR (it was not followed by LF, so
                    // it is part of the value).
                    if unquoted_value_has_pending_cr {
                        if output_write_cursor >= OUTPUT_BUFFER_BYTES {
                            return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
                        }
                        output_buffer[output_write_cursor] = b'\r';
                        output_write_cursor += 1;
                        unquoted_value_has_pending_cr = false;
                    }
                    if output_write_cursor >= OUTPUT_BUFFER_BYTES {
                        return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
                    }
                    output_buffer[output_write_cursor] = current_byte;
                    output_write_cursor += 1;
                }
            }

            LineScanState::CopyingQuotedValue => {
                if current_byte == b'"' {
                    return Ok((output_buffer, output_write_cursor));
                } else {
                    if output_write_cursor >= OUTPUT_BUFFER_BYTES {
                        return Err(ReadTomlFieldError::RsLsfValueExceedsOutputBuffer);
                    }
                    output_buffer[output_write_cursor] = current_byte;
                    output_write_cursor += 1;
                }
            }

            LineScanState::SkippingToEndOfLine | LineScanState::InCommentToEndOfLine => {
                if current_byte == b'\n' {
                    current_state = LineScanState::AtLineStart;
                }
                // else: ignore the byte.
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Tiny pure helpers (no heap, no state)
// ----------------------------------------------------------------------------

#[inline]
fn is_ascii_space_or_tab_byte(byte_value: u8) -> bool {
    matches!(byte_value, b' ' | b'\t')
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
    /// absolute path. Test-only; may use heap freely.
    fn write_unique_temp_toml(label: &str, contents: &str) -> PathBuf {
        let mut path_buffer = std::env::temp_dir();
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
    fn rslsf_handles_crlf_endings_for_unquoted_value() {
        let test_path = write_unique_temp_toml("crlf_unquoted", "port = 8080\r\nx = 1\r\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "port")
                .expect("should find unquoted value with CRLF terminator");
        // The trailing CR before LF must NOT appear in the value.
        assert_eq!(&output_buffer[..written_length], b"8080");
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
    fn rslsf_quoted_value_finds_at_eof_via_closing_quote() {
        // Quoted value with no trailing newline; closing quote IS present.
        // This is valid per policy.
        let test_path = write_unique_temp_toml("quoted_no_lf", "name = \"dora\"");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("quoted value terminated by closing quote should succeed at EOF");
        assert_eq!(&output_buffer[..written_length], b"dora");
    }

    #[test]
    fn rslsf_unquoted_value_without_newline_is_unterminated() {
        // Unquoted value with no trailing newline must now ERROR
        // (newline-required policy).
        let test_path = write_unique_temp_toml("unquoted_no_lf", "port = 8080");
        let result =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "port");
        assert_eq!(
            result,
            Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile),
        );
    }

    #[test]
    fn rslsf_quoted_value_without_closing_quote_is_unterminated() {
        let test_path = write_unique_temp_toml("quoted_no_close", "name = \"alice\n");
        let result =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name");
        // Note: '\n' inside a quoted value is currently treated as data, not
        // a terminator. So this scans through the newline still in the
        // quoted-value state, hits EOF, and reports unterminated.
        assert_eq!(
            result,
            Err(ReadTomlFieldError::RsLsfValueUnterminatedAtEndOfFile),
        );
    }

    #[test]
    fn rslsf_finds_key_after_many_unrelated_lines() {
        // Force the scanner to traverse a large number of unrelated lines
        // before reaching the target key.
        let mut contents = String::new();
        for i in 0..200 {
            contents.push_str(&format!("noise_key_{:03} = \"junkjunkjunk\"\n", i));
        }
        contents.push_str("target = \"eve\"\n");
        let test_path = write_unique_temp_toml("many_lines", &contents);
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(
                path_as_str(&test_path),
                "target",
            )
            .expect("should find value after many lines");
        assert_eq!(&output_buffer[..written_length], b"eve");
    }

    #[test]
    fn rslsf_unrelated_long_line_does_not_abort_scan() {
        // A line far longer than any previous "line buffer" must be handled
        // without aborting: there is no line buffer anymore, so the
        // limiting factor is only the output buffer (used only for the
        // matched value).
        let mut contents = String::new();
        contents.push_str("other_key = \"");
        for _ in 0..4096 {
            contents.push('X');
        }
        contents.push_str("\"\n");
        contents.push_str("name = \"frank\"\n");
        let test_path = write_unique_temp_toml("unrelated_long_line", &contents);
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("unrelated long line should not block finding the real key");
        assert_eq!(&output_buffer[..written_length], b"frank");
    }

    #[test]
    fn rslsf_handles_whitespace_around_key_and_equals() {
        let test_path = write_unique_temp_toml("whitespace", "   name\t=\t  \"grace\"   \n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find value with varied whitespace");
        assert_eq!(&output_buffer[..written_length], b"grace");
    }

    #[test]
    fn rslsf_first_match_wins_when_key_appears_twice() {
        let test_path =
            write_unique_temp_toml("duplicate_key", "name = \"first\"\nname = \"second\"\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("should find first value");
        assert_eq!(&output_buffer[..written_length], b"first");
    }

    #[test]
    fn rslsf_zero_sized_output_buffer_is_rejected() {
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

    #[test]
    fn rslsf_unquoted_value_with_trailing_whitespace_includes_whitespace() {
        // Policy: trailing whitespace before '\n' IS included in the value.
        // This documents the strict-A choice. If the caller wants trimming,
        // they trim.
        let test_path = write_unique_temp_toml("unquoted_trailing_ws", "port = 8080   \n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "port")
                .expect("should find unquoted value");
        assert_eq!(&output_buffer[..written_length], b"8080   ");
    }

    #[test]
    fn rslsf_empty_unquoted_value_returns_zero_length() {
        // "key = \n" — empty unquoted value with newline. Returns OK, len 0.
        let test_path = write_unique_temp_toml("empty_unquoted", "name = \n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("empty unquoted value with newline is valid");
        assert_eq!(written_length, 0);
        // Buffer untouched.
        assert_eq!(output_buffer[0], 0);
    }

    #[test]
    fn rslsf_empty_quoted_value_returns_zero_length() {
        let test_path = write_unique_temp_toml("empty_quoted", "name = \"\"\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "name")
                .expect("empty quoted value is valid");
        assert_eq!(written_length, 0);
        assert_eq!(output_buffer[0], 0);
    }

    #[test]
    fn rslsf_single_byte_key_works() {
        // Cover the special-case path where key length is 1 (transitions
        // straight from AtLineStart to AwaitingEquals).
        let test_path = write_unique_temp_toml("single_byte_key", "x = \"yes\"\n");
        let (output_buffer, written_length) =
            read_single_line_string_field_from_toml_no_heap::<16>(path_as_str(&test_path), "x")
                .expect("single-byte key should work");
        assert_eq!(&output_buffer[..written_length], b"yes");
    }
}

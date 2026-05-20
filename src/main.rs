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

Note:
case by case for need, these can be changed (to be larger or smaller-efficient values)

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

    // read demo file
    print_field_or_terse_error("text", &"test.toml".to_string());
    print_field_or_terse_error("longtext", &"test.toml".to_string());

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

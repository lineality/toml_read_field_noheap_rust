### Example Implimentation of Read-Functionality
- types of read fields
- format of config file
- writing config file

```rust

// ============================================================================
// HIX config files (run configuration in the TOML subset)
// ============================================================================
//
// Purpose
//   A config file holds what is needed to repeat a run: the target crate and
//   binary, the `modeling_data` folder, the scenario name, and analysis
//   options. The wizard writes it once and reads it back to check it. The
//   wizard (replay) and `hix --config` (automated replay) read it.
//
// Extent of this section
//   From this header to the closing brace of `load_config_file`:
//   1. Constants: format version, sizes, file-name parts, keys.
//   2. Value types: `ScenarioName`, `CargoBinaryName`, `AnalysisPurpose`,
//      `ConfigFieldPath`. Any value that can be constructed can be written
//      to a config file and read back unchanged.
//   3. `HixConfig` (one config file) and `config_file_name_for_scenario`.
//   4. Writing: `config_render_error`, `write_config_lines`,
//      `render_config_file_text`, `write_new_config_file`.
//   5. Reading: `read_config_value`, `check_config_format_version`, the
//      `parse_config_*` functions, `load_config_file`.
//   Used from elsewhere in this file: the RSLF field reader, `TopNCount`,
//   `WorkUnitName`, `AbsolutePath`, `is_valid_metadata_text`,
//   `OpenErrorCodes`, `FileOpenPurpose`, `open_file_with_retry`.
//
// Design rules
//   - The reader defines the format. The writer is checked against the
//     reader, never trusted: every written file is read back with
//     `load_config_file` and compared with what was meant.
//   - Render first, then touch disk: the whole file is formatted into a
//     stack buffer before the file is created, so a formatting failure
//     never leaves a file behind.
//   - Never overwrite, never delete: files are created create-new. If the
//     write, sync, or read-back fails after creation, the file stays and the
//     returned code says not to use it.
//   - Memory: config values are a few short `String`s (heap; small and
//     bounded by the MAX_* constants). The render buffer and every read
//     buffer are stack arrays. The field reader uses no heap.
//
// File name: `config_files/hixtoml_<scenario>.toml`, in the directory HIX is
// run from, next to `modeling_data/`. The distinctive prefix lets the wizard
// list config files without picking up other TOML files; any listed file is
// still fully checked by `load_config_file`. One config per scenario name:
// a second config with the same scenario name is refused (create-new).
//
// File layout (exactly as written by `write_config_lines`):
//   # two comment lines
//   hix_config_version = 1
//   target_crate_path = "/ABSOLUTE/path/to/target/crate"
//   target_binary_name = "pumpkin"
//   modeling_data_path = "/ABSOLUTE/path/to/modeling_data"
//   scenario_name = "pumpkin_world_spawn"
//   analysis_purpose = "count_baseline"
//   top_n = 20
//   work_unit_name = "chunks_generated"     <- line omitted when not set
//   demangle_outputs = true
//
// Reading rules (`load_config_file`):
//   - Each known key may appear at most once (816).
//   - `hix_config_version` must equal `CONFIG_FORMAT_VERSION` (831, 832).
//   - Every key except `work_unit_name` is required (811 if missing).
//   - Unknown keys are ignored: only known keys are looked up.
//   - Numbers and booleans may have spaces around them; quoted values are
//     taken exactly as written.
//
// The work-unit COUNT is not stored here: it is a result of each run,
// entered after the run, and belongs in that run's record.
//
// Error codes: 164-169 (the three name types), 817-838 (config files).

/// Format version written to and required from every config file.
/// Raise it when a change to keys or their meaning would make an older HIX
/// misread a newer file (or the reverse); an unequal version is refused (832).
const CONFIG_FORMAT_VERSION: u32 = 1;

/// Longest path value accepted in a config file, in bytes.
///
/// 4096 is the Linux `PATH_MAX`. If a longer path is ever needed, raise this
/// constant; the render buffer (`CONFIG_FILE_MAX_BYTES`) and the path read
/// buffer (this constant, used as a const generic) follow it automatically.
const MAX_CONFIG_PATH_BYTES: usize = 4096;

/// Stack buffer for rendering one config file: the two path values at their
/// maximum length, plus a fixed allowance for comments, keys, and the short
/// fields. The test `cfg_write_new_config_file_longest_values_round_trip`
/// writes a config with every field at its maximum length.
const CONFIG_FILE_MAX_BYTES: usize = 2 * MAX_CONFIG_PATH_BYTES + 4096;

/// Stack buffer for reading short config values (names, numbers, booleans).
/// At least as long as every short field's maximum (compile-time check below).
const CONFIG_SHORT_VALUE_BUFFER_BYTES: usize = 128;

/// Longest scenario name (the `<scenario>` in `run_NNN_<scenario>`).
const MAX_SCENARIO_NAME_BYTES: usize = 96;

/// Longest cargo binary name (passed to `--bin`; cargo validates it further).
const MAX_CARGO_BINARY_NAME_BYTES: usize = 64;

/// Longest analysis purpose (the `<purpose>` in `analysis_NNN_<purpose>`).
const MAX_ANALYSIS_PURPOSE_BYTES: usize = 64;

// Compile-time guard: a run folder name `run_NNNN_<scenario>` (run numbers
// up to 9999) fits in a run label, so the folder name can be passed to the
// analysis as `--run-label` unchanged.
const _: () = assert!("run_9999_".len() + MAX_SCENARIO_NAME_BYTES <= MAX_RUN_LABEL_BYTES);

// Compile-time guard: the short-value read buffer holds every short field at
// its maximum, so an over-long hand-edited name reaches its own type's check
// (with its own code) instead of the reader's generic 812.
const _: () = assert!(
    CONFIG_SHORT_VALUE_BUFFER_BYTES >= MAX_SCENARIO_NAME_BYTES
        && CONFIG_SHORT_VALUE_BUFFER_BYTES >= MAX_CARGO_BINARY_NAME_BYTES
        && CONFIG_SHORT_VALUE_BUFFER_BYTES >= MAX_ANALYSIS_PURPOSE_BYTES
        && CONFIG_SHORT_VALUE_BUFFER_BYTES >= MAX_WORK_UNIT_NAME_BYTES
);

/// Config file names: `hixtoml_<scenario>.toml` (see the section comment).
const CONFIG_FILE_NAME_PREFIX: &str = "hixtoml_";
const CONFIG_FILE_NAME_SUFFIX: &str = ".toml";

/// Config keys (the only place each key's text is written).
const CONFIG_KEY_FORMAT_VERSION: &str = "hix_config_version";
const CONFIG_KEY_TARGET_CRATE_PATH: &str = "target_crate_path";
const CONFIG_KEY_TARGET_BINARY_NAME: &str = "target_binary_name";
const CONFIG_KEY_MODELING_DATA_PATH: &str = "modeling_data_path";
const CONFIG_KEY_SCENARIO_NAME: &str = "scenario_name";
const CONFIG_KEY_ANALYSIS_PURPOSE: &str = "analysis_purpose";
const CONFIG_KEY_TOP_N: &str = "top_n";
const CONFIG_KEY_WORK_UNIT_NAME: &str = "work_unit_name";
const CONFIG_KEY_DEMANGLE_OUTPUTS: &str = "demangle_outputs";

/// Number of config keys.
const CONFIG_KEY_COUNT: usize = 9;

/// Every config key: checked for duplicates before any field is read.
/// The test `cfg_render_config_file_text_writes_every_key_once` keeps this
/// list and the writer in step.
const CONFIG_KEYS: [&str; CONFIG_KEY_COUNT] = [
    CONFIG_KEY_FORMAT_VERSION,
    CONFIG_KEY_TARGET_CRATE_PATH,
    CONFIG_KEY_TARGET_BINARY_NAME,
    CONFIG_KEY_MODELING_DATA_PATH,
    CONFIG_KEY_SCENARIO_NAME,
    CONFIG_KEY_ANALYSIS_PURPOSE,
    CONFIG_KEY_TOP_N,
    CONFIG_KEY_WORK_UNIT_NAME,
    CONFIG_KEY_DEMANGLE_OUTPUTS,
];

/// Open-error codes for creating a config file (create-new: never overwrites).
const CONFIG_WRITE_OPEN_ERROR_CODES: OpenErrorCodes = OpenErrorCodes {
    not_found: HeatmapError::CfgWriteOpenNotFound,
    permission_denied: HeatmapError::CfgWriteOpenPermissionDenied,
    transient: HeatmapError::CfgWriteOpenTransient,
    already_exists: HeatmapError::CfgWriteOpenAlreadyExists,
    other: HeatmapError::CfgWriteOpenOther,
};

// ----------------------------------------------------------------------------
// Name values (scenario, cargo binary, analysis purpose)
// ----------------------------------------------------------------------------
//
// Three separate types, one per meaning, so a scenario name can never be
// passed where a binary name is expected. All three use the run-label rule
// `is_valid_metadata_text`: 1..=max bytes of `A-Z a-z 0-9 _ - .`. That makes
// every name safe as part of a folder or file name (no `/`, no spaces), as a
// quoted config value (no `"`, no `\`), and in the report and TSV (no tabs or
// newlines). Each type has its own maximum, its own two codes, and its own
// method prefix.
//
// Codes 164/166/168 (invalid) are returned both for wizard answers and for
// values loaded from a config file; none of them is a command-line usage
// error, so `main` never shows the usage text for them.

/// Scenario name, e.g. `pumpkin_world_spawn`: names the run folders
/// (`run_NNN_<scenario>`) and the config file (`hixtoml_<scenario>.toml`).
///
/// Field is private: construct only with `scn_new`, read only with `scn_get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioName {
    scenario_name_text: String,
}

impl ScenarioName {
    /// Validates and constructs.
    ///
    /// # Returns
    /// - `Ok(ScenarioName)`.
    /// - `Err(TypeScenarioNameInvalid)` (input validation: the name comes
    ///   from the wizard or from a hand-editable config file): empty, longer
    ///   than `MAX_SCENARIO_NAME_BYTES`, or a byte outside `A-Z a-z 0-9 _ - .`.
    ///
    /// # Modes
    /// Debug builds print the rejected text. Production-release returns the code only.
    pub fn scn_new(candidate_text: &str) -> Result<Self, HeatmapError> {
        if !is_valid_metadata_text(candidate_text, MAX_SCENARIO_NAME_BYTES) {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: invalid scenario name {:?} (1..={} of A-Z a-z 0-9 _ - .)",
                HeatmapError::TypeScenarioNameInvalid.code(),
                candidate_text,
                MAX_SCENARIO_NAME_BYTES
            );
            return Err(HeatmapError::TypeScenarioNameInvalid);
        }
        Ok(Self {
            scenario_name_text: candidate_text.to_string(),
        })
    }

    /// Returns the name after rechecking it.
    ///
    /// # Returns
    /// - `Ok(&str)`.
    /// - `Err(TypeScenarioNameCorrupted)` (internal invariant: `scn_new`
    ///   checked the same rule, so a failure means the stored text changed
    ///   after construction, e.g. memory corruption).
    ///
    /// # Modes
    /// Debug builds (not test) panic; test and debug builds print;
    /// production-release returns the code only.
    pub fn scn_get(&self) -> Result<&str, HeatmapError> {
        if !is_valid_metadata_text(&self.scenario_name_text, MAX_SCENARIO_NAME_BYTES) {
            #[cfg(all(debug_assertions, not(test)))]
            debug_assert!(
                false,
                "HIX-{}: ScenarioName corrupted",
                HeatmapError::TypeScenarioNameCorrupted.code()
            );
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: ScenarioName recheck failed",
                HeatmapError::TypeScenarioNameCorrupted.code()
            );
            return Err(HeatmapError::TypeScenarioNameCorrupted);
        }
        Ok(self.scenario_name_text.as_str())
    }
}

/// Cargo binary name, e.g. `pumpkin`: the target program run with
/// `cargo llvm-cov run --bin <name>`. Needed for workspaces with several binaries.
///
/// Field is private: construct only with `cbn_new`, read only with `cbn_get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoBinaryName {
    cargo_binary_name_text: String,
}

impl CargoBinaryName {
    /// Validates and constructs.
    ///
    /// # Returns
    /// - `Ok(CargoBinaryName)`.
    /// - `Err(TypeCargoBinaryNameInvalid)` (input validation): empty, longer
    ///   than `MAX_CARGO_BINARY_NAME_BYTES`, or a byte outside `A-Z a-z 0-9 _ - .`.
    ///
    /// # Modes
    /// Debug builds print the rejected text. Production-release returns the code only.
    pub fn cbn_new(candidate_text: &str) -> Result<Self, HeatmapError> {
        if !is_valid_metadata_text(candidate_text, MAX_CARGO_BINARY_NAME_BYTES) {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: invalid cargo binary name {:?} (1..={} of A-Z a-z 0-9 _ - .)",
                HeatmapError::TypeCargoBinaryNameInvalid.code(),
                candidate_text,
                MAX_CARGO_BINARY_NAME_BYTES
            );
            return Err(HeatmapError::TypeCargoBinaryNameInvalid);
        }
        Ok(Self {
            cargo_binary_name_text: candidate_text.to_string(),
        })
    }

    /// Returns the name after rechecking it.
    ///
    /// # Returns
    /// - `Ok(&str)`.
    /// - `Err(TypeCargoBinaryNameCorrupted)` (internal invariant; see `scn_get`).
    ///
    /// # Modes
    /// Debug builds (not test) panic; test and debug builds print;
    /// production-release returns the code only.
    pub fn cbn_get(&self) -> Result<&str, HeatmapError> {
        if !is_valid_metadata_text(&self.cargo_binary_name_text, MAX_CARGO_BINARY_NAME_BYTES) {
            #[cfg(all(debug_assertions, not(test)))]
            debug_assert!(
                false,
                "HIX-{}: CargoBinaryName corrupted",
                HeatmapError::TypeCargoBinaryNameCorrupted.code()
            );
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: CargoBinaryName recheck failed",
                HeatmapError::TypeCargoBinaryNameCorrupted.code()
            );
            return Err(HeatmapError::TypeCargoBinaryNameCorrupted);
        }
        Ok(self.cargo_binary_name_text.as_str())
    }
}

/// Analysis purpose, e.g. `count_baseline`: names the analysis folder
/// (`analysis_NNN_<purpose>`) made after each run.
///
/// Field is private: construct only with `apu_new`, read only with `apu_get`.
/// (Prefix `apu_`, because `ap_` belongs to `AbsolutePath`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisPurpose {
    analysis_purpose_text: String,
}

impl AnalysisPurpose {
    /// Validates and constructs.
    ///
    /// # Returns
    /// - `Ok(AnalysisPurpose)`.
    /// - `Err(TypeAnalysisPurposeInvalid)` (input validation): empty, longer
    ///   than `MAX_ANALYSIS_PURPOSE_BYTES`, or a byte outside `A-Z a-z 0-9 _ - .`.
    ///
    /// # Modes
    /// Debug builds print the rejected text. Production-release returns the code only.
    pub fn apu_new(candidate_text: &str) -> Result<Self, HeatmapError> {
        if !is_valid_metadata_text(candidate_text, MAX_ANALYSIS_PURPOSE_BYTES) {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: invalid analysis purpose {:?} (1..={} of A-Z a-z 0-9 _ - .)",
                HeatmapError::TypeAnalysisPurposeInvalid.code(),
                candidate_text,
                MAX_ANALYSIS_PURPOSE_BYTES
            );
            return Err(HeatmapError::TypeAnalysisPurposeInvalid);
        }
        Ok(Self {
            analysis_purpose_text: candidate_text.to_string(),
        })
    }

    /// Returns the purpose after rechecking it.
    ///
    /// # Returns
    /// - `Ok(&str)`.
    /// - `Err(TypeAnalysisPurposeCorrupted)` (internal invariant; see `scn_get`).
    ///
    /// # Modes
    /// Debug builds (not test) panic; test and debug builds print;
    /// production-release returns the code only.
    pub fn apu_get(&self) -> Result<&str, HeatmapError> {
        if !is_valid_metadata_text(&self.analysis_purpose_text, MAX_ANALYSIS_PURPOSE_BYTES) {
            #[cfg(all(debug_assertions, not(test)))]
            debug_assert!(
                false,
                "HIX-{}: AnalysisPurpose corrupted",
                HeatmapError::TypeAnalysisPurposeCorrupted.code()
            );
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: AnalysisPurpose recheck failed",
                HeatmapError::TypeAnalysisPurposeCorrupted.code()
            );
            return Err(HeatmapError::TypeAnalysisPurposeCorrupted);
        }
        Ok(self.analysis_purpose_text.as_str())
    }
}

// ----------------------------------------------------------------------------
// Path values stored in a config file
// ----------------------------------------------------------------------------

/// True if `candidate_text` can be written as a quoted config value and read
/// back byte for byte by the RSLF field reader.
///
/// This checks the characters of the path only; it does not touch the
/// filesystem (the path need not exist).
///
/// Rules, and why each exists:
/// - 1..=`MAX_CONFIG_PATH_BYTES` bytes: the read buffer for path fields has
///   exactly this size, so a longer value would be refused when read (812).
/// - no `"`: it would end the quoted value early.
/// - no `\`: an escape character in real TOML. Refused so the file stays
///   valid TOML and any other TOML tool reads the same value.
/// - no control bytes (0x00-0x1F, 0x7F): a newline would split the line;
///   the others cannot be seen or typed in a hand-edited file.
///
/// Non-ASCII UTF-8 (e.g. accented folder names) is allowed; the reader copies
/// it byte for byte.
fn is_storable_config_field_path_text(candidate_text: &str) -> bool {
    !candidate_text.is_empty()
        && candidate_text.len() <= MAX_CONFIG_PATH_BYTES
        && candidate_text
            .bytes()
            .all(|byte| byte != b'"' && byte != b'\\' && byte >= 0x20 && byte != 0x7F)
}

/// An absolute filesystem path stored as a field VALUE inside a config file
/// (`target_crate_path`, `modeling_data_path`).
///
/// Not the path OF a config file: that is an `AbsolutePath`, passed to
/// `write_new_config_file` and `load_config_file`.
///
/// # Project context
/// A config file is text, so the path is stored as UTF-8 text that satisfies
/// `is_storable_config_field_path_text`. Any value of this type can be written
/// to a config file and read back unchanged. The path is stored exactly as
/// given (not normalized), and nothing is checked on disk: whether it exists
/// is checked by the step that uses it (e.g. running cargo in the target crate).
///
/// Field is private: construct only with `cfp_new`, read only with `cfp_get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFieldPath {
    /// The path's own characters, e.g. `/home/user/code/Pumpkin`.
    field_path_text: String,
}

impl ConfigFieldPath {
    /// Validates and constructs.
    ///
    /// # Arguments
    /// - `candidate_path`: from a wizard answer, from the working directory,
    ///   or from a config file being loaded.
    ///
    /// # Returns
    /// - `Ok(ConfigFieldPath)`.
    /// - `Err(CfgPathValueNotUtf8)` (input validation): a Linux path may hold
    ///   bytes that are not UTF-8; such a path cannot be written as text.
    ///   (Values loaded from a file never reach this: their bytes were
    ///   already checked as UTF-8, code 830.)
    /// - `Err(CfgPathNotAbsolute)` (input validation).
    /// - `Err(CfgPathValueNotStorable)` (input validation): empty, longer than
    ///   `MAX_CONFIG_PATH_BYTES`, or contains `"`, `\`, or a control byte.
    ///
    /// # Modes
    /// Debug builds print the rejected path. Production-release returns the code only.
    pub fn cfp_new(candidate_path: &Path) -> Result<Self, HeatmapError> {
        // --- Check: path is UTF-8 text ---
        // input validation: paths come from the user, the OS, or a file.
        let candidate_text = match candidate_path.to_str() {
            Some(text) => text,
            None => {
                #[cfg(debug_assertions)]
                eprintln!(
                    "HIX-{}: config field path is not UTF-8: {}",
                    HeatmapError::CfgPathValueNotUtf8.code(),
                    candidate_path.display()
                );
                return Err(HeatmapError::CfgPathValueNotUtf8);
            }
        };

        // --- Check: path is absolute ---
        // input validation: HIX uses absolute paths only.
        if !candidate_path.is_absolute() {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: config field path is not absolute: {:?}",
                HeatmapError::CfgPathNotAbsolute.code(),
                candidate_text
            );
            return Err(HeatmapError::CfgPathNotAbsolute);
        }

        // --- Check: path text can be stored and read back unchanged ---
        // input validation: see `is_storable_config_field_path_text`.
        if !is_storable_config_field_path_text(candidate_text) {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: config field path not storable ({} bytes, max {}; no \" \\ or control bytes): {:?}",
                HeatmapError::CfgPathValueNotStorable.code(),
                candidate_text.len(),
                MAX_CONFIG_PATH_BYTES,
                candidate_text
            );
            return Err(HeatmapError::CfgPathValueNotStorable);
        }

        Ok(Self {
            field_path_text: candidate_text.to_string(),
        })
    }

    /// Returns the stored path's characters (e.g. `/home/user/code/Pumpkin`)
    /// after rechecking them. No file is opened or read.
    ///
    /// The one accessor for this value. Callers that need a `&Path` wrap the
    /// result: `Path::new(field_path.cfp_get()?)`.
    ///
    /// # Returns
    /// - `Ok(&str)`.
    /// - `Err(CfgPathValueCorrupted)` (internal invariant: `cfp_new` checked
    ///   the same rules, so a failure means the stored text changed after
    ///   construction, e.g. memory corruption).
    ///
    /// # Modes
    /// Debug builds (not test) panic; test and debug builds print;
    /// production-release returns the code only.
    pub fn cfp_get(&self) -> Result<&str, HeatmapError> {
        let is_intact = is_storable_config_field_path_text(&self.field_path_text)
            && Path::new(&self.field_path_text).is_absolute();
        if !is_intact {
            #[cfg(all(debug_assertions, not(test)))]
            debug_assert!(
                false,
                "HIX-{}: ConfigFieldPath corrupted",
                HeatmapError::CfgPathValueCorrupted.code()
            );
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: ConfigFieldPath recheck failed",
                HeatmapError::CfgPathValueCorrupted.code()
            );
            return Err(HeatmapError::CfgPathValueCorrupted);
        }
        Ok(self.field_path_text.as_str())
    }
}

// ----------------------------------------------------------------------------
// The config
// ----------------------------------------------------------------------------

/// Everything needed to repeat a run (one config file).
///
/// Every field is a validated type, so any `HixConfig` can be written and
/// read back unchanged. It is built with a struct literal inside this module
/// (the wizard, `load_config_file`, tests); its fields are private to this
/// module.
///
/// Adding a field: add a key constant, add it to `CONFIG_KEYS` (and
/// `CONFIG_KEY_COUNT`), write it in `write_config_lines`, read it in
/// `load_config_file`, update the layout test, and consider raising
/// `CONFIG_FORMAT_VERSION`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HixConfig {
    /// Crate directory where cargo commands for the target run.
    target_crate_path: ConfigFieldPath,
    /// Binary to run (`--bin`).
    target_binary_name: CargoBinaryName,
    /// Directory holding the `run_NNN_<scenario>` folders.
    modeling_data_path: ConfigFieldPath,
    /// Scenario: names the run folders and this config file.
    scenario_name: ScenarioName,
    /// Purpose of the analysis made after each run (`analysis_NNN_<purpose>`).
    analysis_purpose: AnalysisPurpose,
    /// Rows per Top-N table in the report.
    top_n: TopNCount,
    /// Name of the work unit (e.g. `chunks_generated`); the count is per run.
    work_unit_name: Option<WorkUnitName>,
    /// Also write `rustfilt`-demangled copies of the report and TSV.
    demangle_outputs: bool,
}

/// Config file name for a scenario: `hixtoml_<scenario>.toml`.
///
/// # Returns
/// - `Ok(file_name)`.
/// - `Err(TypeScenarioNameCorrupted)` from the name's recheck.
fn config_file_name_for_scenario(scenario_name: &ScenarioName) -> Result<String, HeatmapError> {
    let scenario_text = scenario_name.scn_get()?;
    let name_length = CONFIG_FILE_NAME_PREFIX
        .len()
        .saturating_add(scenario_text.len())
        .saturating_add(CONFIG_FILE_NAME_SUFFIX.len());
    let mut file_name = String::with_capacity(name_length);
    file_name.push_str(CONFIG_FILE_NAME_PREFIX);
    file_name.push_str(scenario_text);
    file_name.push_str(CONFIG_FILE_NAME_SUFFIX);
    Ok(file_name)
}

// ----------------------------------------------------------------------------
// Writing
// ----------------------------------------------------------------------------

/// Maps a failed write into the render buffer to its code.
///
/// Internal invariant: `CONFIG_FILE_MAX_BYTES` holds a config with every
/// field at its maximum (checked by test), so this happens only with a
/// smaller buffer (tests) or after a key or field was added without raising
/// the size.
fn config_render_error(_io_error: std::io::Error) -> HeatmapError {
    #[cfg(all(debug_assertions, not(test)))]
    debug_assert!(
        false,
        "HIX-{}: config render buffer full: {}",
        HeatmapError::CfgRenderBufferFull.code(),
        _io_error
    );
    #[cfg(debug_assertions)]
    eprintln!(
        "HIX-{}: config render buffer full: {}",
        HeatmapError::CfgRenderBufferFull.code(),
        _io_error
    );
    HeatmapError::CfgRenderBufferFull
}

/// Writes the config file lines in the layout shown in the section comment.
///
/// # Returns
/// - `Ok(())`.
/// - `Err(CfgRenderBufferFull)` from `config_render_error`.
/// - `Err` from a field's recheck (`CfgPathValueCorrupted`, `Type*Corrupted`).
fn write_config_lines<W: Write>(
    config_writer: &mut W,
    config: &HixConfig,
) -> Result<(), HeatmapError> {
    writeln!(
        config_writer,
        "# HIX config file (TOML subset: one `key = value` per line, top level only)."
    )
    .map_err(config_render_error)?;
    writeln!(
        config_writer,
        "# Written by HIX. Hand edits are allowed; values may not contain double quotes or backslashes."
    )
    .map_err(config_render_error)?;
    writeln!(
        config_writer,
        "{} = {}",
        CONFIG_KEY_FORMAT_VERSION, CONFIG_FORMAT_VERSION
    )
    .map_err(config_render_error)?;
    writeln!(
        config_writer,
        "{} = \"{}\"",
        CONFIG_KEY_TARGET_CRATE_PATH,
        config.target_crate_path.cfp_get()?
    )
    .map_err(config_render_error)?;
    writeln!(
        config_writer,
        "{} = \"{}\"",
        CONFIG_KEY_TARGET_BINARY_NAME,
        config.target_binary_name.cbn_get()?
    )
    .map_err(config_render_error)?;
    writeln!(
        config_writer,
        "{} = \"{}\"",
        CONFIG_KEY_MODELING_DATA_PATH,
        config.modeling_data_path.cfp_get()?
    )
    .map_err(config_render_error)?;
    writeln!(
        config_writer,
        "{} = \"{}\"",
        CONFIG_KEY_SCENARIO_NAME,
        config.scenario_name.scn_get()?
    )
    .map_err(config_render_error)?;
    writeln!(
        config_writer,
        "{} = \"{}\"",
        CONFIG_KEY_ANALYSIS_PURPOSE,
        config.analysis_purpose.apu_get()?
    )
    .map_err(config_render_error)?;
    writeln!(
        config_writer,
        "{} = {}",
        CONFIG_KEY_TOP_N,
        config.top_n.tnc_get()?
    )
    .map_err(config_render_error)?;
    if let Some(work_unit_name) = &config.work_unit_name {
        writeln!(
            config_writer,
            "{} = \"{}\"",
            CONFIG_KEY_WORK_UNIT_NAME,
            work_unit_name.wun_get()?
        )
        .map_err(config_render_error)?;
    }
    writeln!(
        config_writer,
        "{} = {}",
        CONFIG_KEY_DEMANGLE_OUTPUTS, config.demangle_outputs
    )
    .map_err(config_render_error)?;
    Ok(())
}

/// Renders the config file text into `output_buffer` (no heap) and returns
/// the written part.
///
/// # Project context
/// The whole file is formatted in memory before the file is created, so a
/// formatting failure never leaves a half-written config on disk. The caller
/// (`write_new_config_file`) passes a `CONFIG_FILE_MAX_BYTES` stack array;
/// tests also pass smaller buffers.
///
/// # Returns
/// - `Ok(rendered_bytes)`: the start of `output_buffer`, holding the file text.
/// - `Err(CfgRenderBufferFull)`: the text does not fit.
/// - `Err(CfgRenderedRangeInvalid)` (internal invariant): the writer's end
///   position does not describe a range inside the buffer.
/// - `Err` from a field's recheck (`CfgPathValueCorrupted`, `Type*Corrupted`).
///
/// # Modes
/// The internal-invariant check panics in debug builds (not test); test and
/// debug builds print; production-release returns the code only.
fn render_config_file_text<'buffer>(
    config: &HixConfig,
    output_buffer: &'buffer mut [u8],
) -> Result<&'buffer [u8], HeatmapError> {
    let mut buffer_writer = std::io::Cursor::new(output_buffer);
    write_config_lines(&mut buffer_writer, config)?;
    let written_end_position = buffer_writer.position();
    let full_buffer: &'buffer [u8] = buffer_writer.into_inner();

    // --- Check: the written end position is a range inside the buffer ---
    // internal invariant: a Cursor never writes past its buffer, so its
    // position is at most the buffer length, which fits in usize.
    let rendered_bytes = match usize::try_from(written_end_position) {
        Ok(written_length) => full_buffer.get(..written_length),
        Err(_conversion_error) => None,
    };
    match rendered_bytes {
        Some(bytes) => Ok(bytes),
        None => {
            #[cfg(all(debug_assertions, not(test)))]
            debug_assert!(
                false,
                "HIX-{}: rendered end {} outside buffer of {} bytes",
                HeatmapError::CfgRenderedRangeInvalid.code(),
                written_end_position,
                full_buffer.len()
            );
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: rendered end {} outside buffer of {} bytes",
                HeatmapError::CfgRenderedRangeInvalid.code(),
                written_end_position,
                full_buffer.len()
            );
            Err(HeatmapError::CfgRenderedRangeInvalid)
        }
    }
}

/// Writes a new config file, then reads it back and compares.
///
/// # Project context
/// Config files are a permanent record of how runs were made, like run and
/// analysis folders, so an existing file is never overwritten. The file is
/// read back with the same no-heap reader that replay uses and compared with
/// what was meant, so a writer/reader mismatch is caught at write time, not
/// at the next replay.
///
/// # Steps
/// 1. Render the whole text into a stack buffer (nothing on disk yet).
/// 2. Create the file (create-new; Tier-1 retry for transient errors),
///    write it with one `write_all`, and sync it to disk.
/// 3. Load it with `load_config_file` and compare with `config`.
///
/// If step 2 or 3 fails after the file was created, the file stays in place
/// (HIX never deletes); the returned code says not to use it.
///
/// # Arguments
/// - `config_file_path`: absolute path of the file to create, normally
///   `config_files/hixtoml_<scenario>.toml`. Its directory must exist (821 if not).
/// - `config`: the config to write.
///
/// # Returns
/// - `Ok(())`.
/// - `Err(821 | 822 | 823 | 824 | 825)`: creating the file failed
///   (824: the file exists; it is not overwritten).
/// - `Err(CfgWriteFailed | CfgSyncFailed)`.
/// - `Err(CfgRoundTripMismatch)` (internal invariant).
/// - `Err` from `render_config_file_text` and `load_config_file`.
///
/// # Modes
/// Debug builds print each failure; the round-trip check also panics in
/// debug builds (not test). Production-release returns the code only.
pub fn write_new_config_file(
    config_file_path: &AbsolutePath,
    config: &HixConfig,
) -> Result<(), HeatmapError> {
    let file_path = config_file_path.ap_get()?;

    let mut render_buffer: [u8; CONFIG_FILE_MAX_BYTES] = [0u8; CONFIG_FILE_MAX_BYTES];
    let rendered_bytes = render_config_file_text(config, &mut render_buffer)?;

    let mut config_file = open_file_with_retry(
        file_path,
        FileOpenPurpose::CreateNew,
        CONFIG_WRITE_OPEN_ERROR_CODES,
    )?;
    if let Err(_io_error) = config_file.write_all(rendered_bytes) {
        #[cfg(debug_assertions)]
        eprintln!(
            "HIX-{}: config write failed: {}",
            HeatmapError::CfgWriteFailed.code(),
            _io_error
        );
        return Err(HeatmapError::CfgWriteFailed);
    }
    if let Err(_io_error) = config_file.sync_all() {
        #[cfg(debug_assertions)]
        eprintln!(
            "HIX-{}: config sync failed: {}",
            HeatmapError::CfgSyncFailed.code(),
            _io_error
        );
        return Err(HeatmapError::CfgSyncFailed);
    }
    // Release the file handle before reading the file back.
    drop(config_file);

    // --- Check: the file reads back as exactly the config written ---
    // internal invariant: the writer and the loader implement the same layout.
    let reloaded_config = load_config_file(config_file_path)?;
    if reloaded_config != *config {
        #[cfg(all(debug_assertions, not(test)))]
        debug_assert!(
            false,
            "HIX-{}: config read-back differs",
            HeatmapError::CfgRoundTripMismatch.code()
        );
        #[cfg(debug_assertions)]
        eprintln!(
            "HIX-{}: config read-back differs: wrote {:?}, read {:?}",
            HeatmapError::CfgRoundTripMismatch.code(),
            config,
            reloaded_config
        );
        return Err(HeatmapError::CfgRoundTripMismatch);
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// Reading
// ----------------------------------------------------------------------------

/// Reads one config value into a `VALUE_BUFFER_BYTES` stack array, checks
/// that it is UTF-8, and hands it to `parse_value`.
///
/// # Returns
/// - `Ok(T)` from `parse_value`.
/// - Errors from the field reader (e.g. 811 missing, 812 too long).
/// - `Err(CfgValueLengthInvalid)` (internal invariant: the reader's length
///   is within its buffer).
/// - `Err(CfgValueNotUtf8)` (input validation: a hand-edited file may hold
///   non-UTF-8 bytes).
/// - Errors from `parse_value`.
fn read_config_value<T, F, const VALUE_BUFFER_BYTES: usize>(
    config_file_path: &Path,
    config_key: &str,
    parse_value: F,
) -> Result<T, HeatmapError>
where
    F: FnOnce(&str) -> Result<T, HeatmapError>,
{
    let (value_buffer, value_length) = read_single_line_field_from_toml_no_heap::<
        VALUE_BUFFER_BYTES,
    >(config_file_path, config_key)?;
    let value_bytes = match value_buffer.get(..value_length) {
        Some(bytes) => bytes,
        None => {
            #[cfg(all(debug_assertions, not(test)))]
            debug_assert!(
                false,
                "HIX-{}: value length {} beyond buffer {}",
                HeatmapError::CfgValueLengthInvalid.code(),
                value_length,
                VALUE_BUFFER_BYTES
            );
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: value length {} beyond buffer {}",
                HeatmapError::CfgValueLengthInvalid.code(),
                value_length,
                VALUE_BUFFER_BYTES
            );
            return Err(HeatmapError::CfgValueLengthInvalid);
        }
    };
    let value_text = match std::str::from_utf8(value_bytes) {
        Ok(text) => text,
        Err(_utf8_error) => {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: value of {:?} is not UTF-8: {}",
                HeatmapError::CfgValueNotUtf8.code(),
                config_key,
                _utf8_error
            );
            return Err(HeatmapError::CfgValueNotUtf8);
        }
    };
    parse_value(value_text)
}

/// Checks `hix_config_version`.
///
/// # Returns
/// - `Ok(())` when the version equals `CONFIG_FORMAT_VERSION`.
/// - `Err(CfgVersionNotANumber)` (input validation): not a u32 number.
/// - `Err(CfgUnsupportedVersion)` (input validation): a different version.
fn check_config_format_version(value_text: &str) -> Result<(), HeatmapError> {
    let format_version = match value_text.trim().parse::<u32>() {
        Ok(version) => version,
        Err(_parse_error) => {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: hix_config_version {:?}: {}",
                HeatmapError::CfgVersionNotANumber.code(),
                value_text,
                _parse_error
            );
            return Err(HeatmapError::CfgVersionNotANumber);
        }
    };
    if format_version != CONFIG_FORMAT_VERSION {
        #[cfg(debug_assertions)]
        eprintln!(
            "HIX-{}: hix_config_version {} (this HIX reads {})",
            HeatmapError::CfgUnsupportedVersion.code(),
            format_version,
            CONFIG_FORMAT_VERSION
        );
        return Err(HeatmapError::CfgUnsupportedVersion);
    }
    Ok(())
}

/// Parses a path field value (`target_crate_path`, `modeling_data_path`).
/// Errors: see `ConfigFieldPath::cfp_new`.
fn parse_config_field_path(value_text: &str) -> Result<ConfigFieldPath, HeatmapError> {
    ConfigFieldPath::cfp_new(Path::new(value_text))
}

/// Parses `top_n`. Config-specific codes, so a bad config is never reported
/// as a command-line mistake (the CLI codes make `main` print usage text).
///
/// # Returns
/// - `Ok(TopNCount)`.
/// - `Err(CfgTopNNotANumber)` (input validation): not a u16 number
///   (this includes numbers above 65535).
/// - `Err(CfgTopNOutOfRange)` (input validation): outside 1..=`MAX_TOP_N`.
///
/// # Modes
/// Debug builds print the value. For out-of-range values, `tnc_new` also
/// prints its own type-level line first; the config line follows it.
fn parse_config_top_n(value_text: &str) -> Result<TopNCount, HeatmapError> {
    let top_n_value = match value_text.trim().parse::<u16>() {
        Ok(value) => value,
        Err(_parse_error) => {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: config top_n {:?} is not a u16 number: {}",
                HeatmapError::CfgTopNNotANumber.code(),
                value_text,
                _parse_error
            );
            return Err(HeatmapError::CfgTopNNotANumber);
        }
    };
    match TopNCount::tnc_new(top_n_value) {
        Ok(top_n) => Ok(top_n),
        Err(_type_error) => {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: config top_n {} outside 1..={}",
                HeatmapError::CfgTopNOutOfRange.code(),
                top_n_value,
                MAX_TOP_N
            );
            Err(HeatmapError::CfgTopNOutOfRange)
        }
    }
}

/// Parses a boolean: exactly `true` or `false` (spaces around allowed).
///
/// # Returns
/// - `Ok(bool)`.
/// - `Err(CfgBooleanInvalid)` (input validation).
fn parse_config_boolean(value_text: &str) -> Result<bool, HeatmapError> {
    match value_text.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: config boolean {:?} is not true or false",
                HeatmapError::CfgBooleanInvalid.code(),
                value_text
            );
            Err(HeatmapError::CfgBooleanInvalid)
        }
    }
}

/// Parses `work_unit_name`. A config-specific code, so a bad config is never
/// reported as a command-line mistake.
///
/// # Returns
/// - `Ok(WorkUnitName)`.
/// - `Err(CfgWorkUnitNameInvalid)` (input validation).
///
/// # Modes
/// Debug builds print the value. `wun_new` also prints its own type-level
/// line first (worded for the `--work-unit-name` flag); the config line follows it.
fn parse_config_work_unit_name(value_text: &str) -> Result<WorkUnitName, HeatmapError> {
    match WorkUnitName::wun_new(value_text) {
        Ok(work_unit_name) => Ok(work_unit_name),
        Err(_type_error) => {
            #[cfg(debug_assertions)]
            eprintln!(
                "HIX-{}: config work_unit_name {:?} (1..={} of A-Z a-z 0-9 _ - .)",
                HeatmapError::CfgWorkUnitNameInvalid.code(),
                value_text,
                MAX_WORK_UNIT_NAME_BYTES
            );
            Err(HeatmapError::CfgWorkUnitNameInvalid)
        }
    }
}

/// Loads and validates a config file.
///
/// # Steps
/// 1. Refuse duplicate keys (816), so no value is silently ignored.
/// 2. Require `hix_config_version` == `CONFIG_FORMAT_VERSION`.
/// 3. Read each field into a stack array sized to its maximum and build its
///    validated type. `work_unit_name` is optional (missing = `None`); all
///    others are required.
///
/// Cost: about two file opens per key, each read one byte per read call.
/// Negligible for a file of about a dozen lines.
///
/// # Returns
/// - `Ok(HixConfig)`.
/// - `Err`: 155 (the file path's own recheck); field-reader codes 801-816
///   (811: a required key is missing; 812: a value is longer than its
///   buffer); 817, 818, 837 (paths); 830-835, 838; 164/166/168 (names).
pub fn load_config_file(config_file_path: &AbsolutePath) -> Result<HixConfig, HeatmapError> {
    let file_path = config_file_path.ap_get()?;
    check_toml_keys_are_unique_no_heap(file_path, &CONFIG_KEYS)?;

    read_config_value::<_, _, CONFIG_SHORT_VALUE_BUFFER_BYTES>(
        file_path,
        CONFIG_KEY_FORMAT_VERSION,
        check_config_format_version,
    )?;

    let target_crate_path = read_config_value::<_, _, MAX_CONFIG_PATH_BYTES>(
        file_path,
        CONFIG_KEY_TARGET_CRATE_PATH,
        parse_config_field_path,
    )?;
    let target_binary_name = read_config_value::<_, _, CONFIG_SHORT_VALUE_BUFFER_BYTES>(
        file_path,
        CONFIG_KEY_TARGET_BINARY_NAME,
        CargoBinaryName::cbn_new,
    )?;
    let modeling_data_path = read_config_value::<_, _, MAX_CONFIG_PATH_BYTES>(
        file_path,
        CONFIG_KEY_MODELING_DATA_PATH,
        parse_config_field_path,
    )?;
    let scenario_name = read_config_value::<_, _, CONFIG_SHORT_VALUE_BUFFER_BYTES>(
        file_path,
        CONFIG_KEY_SCENARIO_NAME,
        ScenarioName::scn_new,
    )?;
    let analysis_purpose = read_config_value::<_, _, CONFIG_SHORT_VALUE_BUFFER_BYTES>(
        file_path,
        CONFIG_KEY_ANALYSIS_PURPOSE,
        AnalysisPurpose::apu_new,
    )?;
    let top_n = read_config_value::<_, _, CONFIG_SHORT_VALUE_BUFFER_BYTES>(
        file_path,
        CONFIG_KEY_TOP_N,
        parse_config_top_n,
    )?;
    let work_unit_name = match read_config_value::<_, _, CONFIG_SHORT_VALUE_BUFFER_BYTES>(
        file_path,
        CONFIG_KEY_WORK_UNIT_NAME,
        parse_config_work_unit_name,
    ) {
        Ok(work_unit_name) => Some(work_unit_name),
        // Optional field: absent means "not set".
        Err(HeatmapError::RSLFFieldNotFound) => None,
        Err(other_error) => return Err(other_error),
    };
    let demangle_outputs = read_config_value::<_, _, CONFIG_SHORT_VALUE_BUFFER_BYTES>(
        file_path,
        CONFIG_KEY_DEMANGLE_OUTPUTS,
        parse_config_boolean,
    )?;

    Ok(HixConfig {
        target_crate_path,
        target_binary_name,
        modeling_data_path,
        scenario_name,
        analysis_purpose,
        top_n,
        work_unit_name,
        demangle_outputs,
    })
}

// end hix toml files

```


### Tests

```rust

// ============================================================================
// Tests (test mode only)
// ============================================================================

/// Unit tests (compiled only under `cargo test`).
///
/// Most tests use in-memory input (`std::io::Cursor`) and synthetic sites.
/// Tests of output safety (codes 510-515), of the loop scan on real files,
/// and of config files create uniquely named entries under
/// `std::env::temp_dir()` and remove them at the end; if such a test fails
/// mid-way, its temp entry may remain (harmless, unique name). Other
/// temp-dir paths are used only as path values (never created or opened).
///
/// Test names: `<area>_<function>_<condition>[_with_code_<nnn>]`.
/// Areas: `cli`, `type`, `lcov`, `src` (loop scan), `ana` (analysis),
/// `rpt` (report output), `cfg` (config files), `err` (error-code policy),
/// `guard` (single-source-of-truth checks on the source text itself).
/// `unwrap`/`assert!`/indexing are allowed here (test mode).
#[cfg(test)]
mod hix_heatmap_indices_for_compute_tests {
    use super::*;
    use std::io::Cursor;

    //...

    /// Config choice: n, a listed number, and rejected answers (727).
    #[test]
    fn wiz_parse_config_choice_answer_with_code_727() {
        let names = vec![
            String::from("hixtoml_a.toml"),
            String::from("hixtoml_b.toml"),
        ];
        assert_eq!(parse_config_choice_answer("n", &names).unwrap(), None);
        assert_eq!(
            parse_config_choice_answer("2", &names).unwrap(),
            Some(String::from("hixtoml_b.toml"))
        );
        for bad_answer in ["0", "3", "x", ""] {
            let error = parse_config_choice_answer(bad_answer, &names).unwrap_err();
            assert_eq!(
                error,
                HeatmapError::WizConfigChoiceInvalid,
                "{:?}",
                bad_answer
            );
            assert_eq!(error.code(), 727);
        }
    }

    /// Only `hixtoml_<valid scenario name>.toml` counts as a config file name.
    #[test]
    fn wiz_is_config_file_name_cases() {
        assert!(is_config_file_name("hixtoml_pumpkin_world_spawn.toml"));
        assert!(!is_config_file_name("hix_pumpkin.toml"));
        assert!(!is_config_file_name("hixtoml_.toml"));
        assert!(!is_config_file_name("hixtoml_a b.toml"));
        assert!(!is_config_file_name("hixtoml_a.toml.bak"));
        assert!(!is_config_file_name("Cargo.toml"));
    }

    /// Run numbers come only from `run_<digits>_...` names.
    #[test]
    fn wiz_run_number_from_folder_name_cases() {
        assert_eq!(run_number_from_folder_name("run_005_pumpkin"), Some(5));
        assert_eq!(run_number_from_folder_name("run_1_x"), Some(1));
        assert_eq!(run_number_from_folder_name("run_0012_a"), Some(12));
        assert_eq!(
            run_number_from_folder_name("run_99999999999_x"),
            Some(u32::MAX)
        );
        assert_eq!(run_number_from_folder_name("run_005"), None);
        assert_eq!(run_number_from_folder_name("run__x"), None);
        assert_eq!(run_number_from_folder_name("test_5"), None);
        assert_eq!(run_number_from_folder_name("analysis_001_x"), None);
    }

    /// The next run number is the highest plus 1; past `MAX_RUN_NUMBER` it is 714.
    #[test]
    fn wiz_next_run_number_highest_plus_one_and_exhausted_with_code_714() {
        let modeling_directory = unique_temp_path("wiz_run_numbers");
        fs::create_dir(&modeling_directory).unwrap();
        assert_eq!(next_run_number(&modeling_directory).unwrap(), 1);
        fs::create_dir(modeling_directory.join("run_004_old")).unwrap();
        fs::create_dir(modeling_directory.join("test_5")).unwrap();
        assert_eq!(next_run_number(&modeling_directory).unwrap(), 5);
        fs::create_dir(modeling_directory.join("run_9999_last")).unwrap();
        let error = next_run_number(&modeling_directory).unwrap_err();
        assert_eq!(error, HeatmapError::WizRunNumberExhausted);
        assert_eq!(error.code(), 714);
        fs::remove_dir_all(&modeling_directory).unwrap();
    }

    /// End to end with a new config: the directories are created, a valid
    /// config is written (defaults used), and run 001 is planned.
    #[test]
    fn wiz_run_wizard_setup_dialog_new_config_plans_run_001() {
        let base_directory = unique_temp_path("wiz_new_config");
        fs::create_dir(&base_directory).unwrap();
        let crate_directory = base_directory.join("target_crate");
        fs::create_dir(&crate_directory).unwrap();
        fs::write(crate_directory.join("Cargo.toml"), "[package]\n").unwrap();

        // base: y; create dirs: y; crate; binary; scenario; purpose default;
        // top_n default; no work unit; demangle: n; write: y.
        let answers = format!(
            "y\ny\n{}\npumpkin\nworld_spawn\n\n\n\nn\ny\n",
            crate_directory.display()
        );
        let mut wizard_input = Cursor::new(answers.into_bytes());
        let mut wizard_output: Vec<u8> = Vec::new();
        run_wizard_setup_dialog(&mut wizard_input, &mut wizard_output, &base_directory).unwrap();
        let output_text = String::from_utf8(wizard_output).unwrap();

        assert!(base_directory.join(MODELING_DATA_DIRECTORY_NAME).is_dir());
        let config_file_path = base_directory
            .join(CONFIG_FILES_DIRECTORY_NAME)
            .join("hixtoml_world_spawn.toml");
        let loaded = load_config_file(&AbsolutePath::ap_new(config_file_path).unwrap()).unwrap();
        assert_eq!(
            loaded.analysis_purpose.apu_get().unwrap(),
            DEFAULT_ANALYSIS_PURPOSE
        );
        assert_eq!(loaded.top_n.tnc_get().unwrap(), DEFAULT_TOP_N);
        assert!(loaded.work_unit_name.is_none());
        assert!(!loaded.demangle_outputs);
        assert!(output_text.contains("run_001_world_spawn"));
        assert!(output_text.contains("--bin pumpkin"));
        assert!(output_text.contains("analysis_001_count_baseline"));

        fs::remove_dir_all(&base_directory).unwrap();
    }

    /// End to end with an existing config: picked by number, and the next
    /// run number follows the highest existing run folder (004 -> 005).
    #[test]
    fn wiz_run_wizard_setup_dialog_existing_config_plans_next_run() {
        let base_directory = unique_temp_path("wiz_existing_config");
        let modeling_directory = base_directory.join(MODELING_DATA_DIRECTORY_NAME);
        let config_directory = base_directory.join(CONFIG_FILES_DIRECTORY_NAME);
        fs::create_dir(&base_directory).unwrap();
        fs::create_dir(&modeling_directory).unwrap();
        fs::create_dir(&config_directory).unwrap();
        fs::create_dir(modeling_directory.join("run_004_old")).unwrap();
        fs::create_dir(modeling_directory.join("test_5")).unwrap();

        let config = HixConfig {
            target_crate_path: ConfigFieldPath::cfp_new(&base_directory.join("crate")).unwrap(),
            target_binary_name: CargoBinaryName::cbn_new("lines").unwrap(),
            modeling_data_path: ConfigFieldPath::cfp_new(&modeling_directory).unwrap(),
            scenario_name: ScenarioName::scn_new("cut_paste").unwrap(),
            analysis_purpose: AnalysisPurpose::apu_new("count_baseline").unwrap(),
            top_n: TopNCount::tnc_new(10).unwrap(),
            work_unit_name: Some(WorkUnitName::wun_new("edits").unwrap()),
            demangle_outputs: true,
        };
        let config_file_path =
            AbsolutePath::ap_new(config_directory.join("hixtoml_cut_paste.toml")).unwrap();
        write_new_config_file(&config_file_path, &config).unwrap();

        let mut wizard_input = Cursor::new(b"y\n1\n".to_vec());
        let mut wizard_output: Vec<u8> = Vec::new();
        run_wizard_setup_dialog(&mut wizard_input, &mut wizard_output, &base_directory).unwrap();
        let output_text = String::from_utf8(wizard_output).unwrap();
        assert!(output_text.contains("using hixtoml_cut_paste.toml"));
        assert!(output_text.contains("run_005_cut_paste"));
        assert!(output_text.contains("--work-unit-name edits"));
        assert!(output_text.contains("rustfilt <"));

        fs::remove_dir_all(&base_directory).unwrap();
    }

    /// Declining directory creation stops with 710 and creates nothing.
    #[test]
    fn wiz_run_wizard_setup_dialog_declined_directories_with_code_710() {
        let base_directory = unique_temp_path("wiz_declined");
        fs::create_dir(&base_directory).unwrap();
        let mut wizard_input = Cursor::new(b"y\nn\n".to_vec());
        let mut wizard_output: Vec<u8> = Vec::new();
        let error = run_wizard_setup_dialog(&mut wizard_input, &mut wizard_output, &base_directory)
            .unwrap_err();
        assert_eq!(error, HeatmapError::WizUserDeclinedDirectoryCreation);
        assert_eq!(error.code(), 710);
        assert!(!base_directory.join(MODELING_DATA_DIRECTORY_NAME).exists());
        assert!(!base_directory.join(CONFIG_FILES_DIRECTORY_NAME).exists());
        fs::remove_dir(&base_directory).unwrap();
    }
}

```

/// Value type of a built-in config option.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ConfigValueType {
    Bool,
    Int,
    Double,
    String,
    Quantity(&'static str), // 300s, 1.5MB TODO: I should also add this to the NedType
}

pub struct BuiltinConfigOption {
    pub name: &'static str,
    pub value_type: ConfigValueType,
    pub description: &'static str,
    #[allow(dead_code)]
    pub per_object: bool,
}

// wup wup → my first macro
macro_rules! global_option {
    // the parameter
    ($name:literal, $ty:expr, $desc:literal) => {
        // the expansion
        BuiltinConfigOption {
            name: $name,
            value_type: $ty,
            description: $desc,
            per_object: false,
        }
    };
}

// imo this is semantically a bit cleaner than adding per_object as a parameter to the macro
macro_rules! per_object_option {
    ($name:literal, $ty:expr, $desc:literal) => {
        BuiltinConfigOption {
            name: $name,
            value_type: $ty,
            description: $desc,
            per_object: true,
        }
    };
}

// avoid ConfigValueType:: below
use ConfigValueType::{Bool, Double, Int, Quantity, String};

// https://raw.githubusercontent.com/omnetpp/omnetpp/master/doc/src/manual/appendix-config-options.tex
// and opp_h
pub static GLOBAL_CONFIG: &[BuiltinConfigOption] = &[
    // general
    global_option!(
        "network",
        String,
        "The name of the network to be simulated."
    ),
    global_option!(
        "extends",
        String,
        "Name of the configuration this section is based on."
    ),
    global_option!(
        "description",
        String,
        "Descriptive name for the configuration."
    ),
    global_option!(
        "fallback-to",
        String,
        "Section to fall back to for configuration lookups."
    ),
    global_option!(
        "abstract",
        Bool,
        "Marks the section as a base for other sections; it cannot be run directly."
    ),
    global_option!(
        "constraint",
        String,
        "Expression that iteration variables must satisfy for a run to be generated."
    ),
    global_option!(
        "iteration-nesting-order",
        String,
        "Nesting order of iteration variables during run generation."
    ),
    global_option!(
        "max-module-nesting",
        Int,
        "Maximum allowed submodule nesting depth (catches infinite recursion)."
    ),
    global_option!(
        "runnumber-width",
        Int,
        "Zero-padding width of the ${runnumber} variable in file names."
    ),
    global_option!(
        "allow-object-stealing-on-deletion",
        Bool,
        "Disable the error on deleting objects owned elsewhere (backward compatibility)."
    ),
    global_option!(
        "parameter-mutability-check",
        Bool,
        "Enforce the @mutable annotation when parameters are changed at runtime."
    ),
    global_option!(
        "print-instantiated-ned-types",
        Bool,
        "Print the NED types instantiated during network setup."
    ),
    global_option!(
        "print-undisposed",
        Bool,
        "Report objects that were not deleted after network cleanup."
    ),
    global_option!(
        "print-unused-parameters",
        Bool,
        "Report ini parameter assignments that were never used."
    ),
    global_option!("warnings", Bool, "Enable warning messages."),
    // run labels
    global_option!("experiment", String, "Older name of experiment-label."),
    global_option!("measurement", String, "Older name of measurement-label."),
    global_option!("replication", String, "Older name of replication-label."),
    global_option!(
        "repeat",
        Int,
        "Number of replications with the same parameters but different seeds."
    ),
    global_option!(
        "experiment-label",
        String,
        "Experiment identifier recorded into result files (default: ${configname})."
    ),
    global_option!(
        "measurement-label",
        String,
        "Measurement identifier recorded into result files (default: ${iterationvars})."
    ),
    global_option!(
        "replication-label",
        String,
        "Replication identifier recorded into result files (default: #${repetition})."
    ),
    // random numbers
    global_option!(
        "num-rngs",
        Int,
        "Number of random number generator instances per run."
    ),
    global_option!(
        "seed-set",
        Int,
        "Selects the seed set for automatic seed assignment (default: ${runnumber})."
    ),
    global_option!(
        "rng-class",
        String,
        "Random number generator class (default: cMersenneTwister)."
    ),
    global_option!(
        "rngmanager-class",
        String,
        "Envir plugin: RNG manager class providing RNGs to modules and channels (must implement cIRngManager)."
    ),
    // time
    global_option!(
        "sim-time-limit",
        Quantity("s"),
        "Stop the simulation when this simulation time is reached."
    ),
    global_option!(
        "cpu-time-limit",
        Quantity("s"),
        "Stop the simulation when this much CPU time has been used."
    ),
    global_option!(
        "real-time-limit",
        Quantity("s"),
        "Stop the simulation after this much elapsed real time."
    ),
    global_option!(
        "warmup-period",
        Quantity("s"),
        "Initial period excluded from result collection."
    ),
    global_option!(
        "simtime-resolution",
        String,
        "Resolution of the 64-bit fixed-point simulation time (e.g. ps, ns, us)."
    ),
    global_option!(
        "simtime-scale",
        Int,
        "Deprecated: base-10 exponent of the simulation time scale."
    ),
    global_option!(
        "realtimescheduler-scaling",
        Double,
        "Simulation speed relative to real time for cRealTimeScheduler."
    ),
    // debugging
    global_option!(
        "debug-on-errors",
        Bool,
        "Break into the debugger on runtime errors."
    ),
    global_option!(
        "debugger-attach-on-startup",
        Bool,
        "Launch and attach a debugger at simulation startup."
    ),
    global_option!(
        "debugger-attach-on-error",
        Bool,
        "Launch and attach a debugger on runtime errors."
    ),
    global_option!(
        "debugger-attach-command",
        String,
        "Command line for launching the debugger (%u is the PID placeholder)."
    ),
    global_option!(
        "debugger-attach-wait-time",
        Quantity("s"),
        "How long to wait for the debugger to start up."
    ),
    global_option!(
        "check-signals",
        Bool,
        "Validate emitted signals against @signal declarations in NED."
    ),
    global_option!(
        "debug-statistics-recording",
        Bool,
        "Print debug information about the statistics recording setup."
    ),
    // files
    global_option!("result-dir", String, "Base directory for result files."),
    global_option!(
        "resultdir-subdivision",
        Bool,
        "Create a hierarchical subdirectory structure under the results directory."
    ),
    global_option!(
        "image-path",
        String,
        "Semicolon-separated list of directories with icons and images."
    ),
    global_option!(
        "fname-append-host",
        Bool,
        "Append hostname and PID to output file names (useful for parallel runs)."
    ),
    global_option!(
        "ned-path",
        String,
        "Semicolon-separated list of NED source folders."
    ),
    global_option!(
        "ned-package-exclusions",
        String,
        "Semicolon-separated list of NED packages to exclude from loading."
    ),
    global_option!("load-libs", String, "Dynamic libraries to load on startup."),
    // cmdenv
    global_option!(
        "cmdenv-autoflush",
        Bool,
        "Flush stdout after each event banner or status update."
    ),
    global_option!(
        "cmdenv-config-name",
        String,
        "Name of the configuration section to run."
    ),
    global_option!(
        "cmdenv-event-banner-details",
        Bool,
        "Print extra information after each event banner."
    ),
    global_option!(
        "cmdenv-event-banners",
        Bool,
        "Print banners for each event (normal mode only)."
    ),
    global_option!(
        "cmdenv-express-mode",
        Bool,
        "Run in express mode (less output, faster)."
    ),
    global_option!(
        "cmdenv-extra-stack",
        Quantity("B"),
        "Extra stack space reserved for each activity() module."
    ),
    global_option!(
        "cmdenv-fake-gui",
        Bool,
        "Pretend to be a GUI so refreshDisplay() gets called periodically."
    ),
    global_option!(
        "cmdenv-fake-gui-after-event-probability",
        Double,
        "Probability of a refreshDisplay() call after each event."
    ),
    global_option!(
        "cmdenv-fake-gui-before-event-probability",
        Double,
        "Probability of a refreshDisplay() call before each event."
    ),
    global_option!(
        "cmdenv-fake-gui-on-hold-numsteps",
        String,
        "Number of refreshDisplay() calls during a hold period."
    ),
    global_option!(
        "cmdenv-fake-gui-on-hold-probability",
        Double,
        "Probability of refreshDisplay() calls during a hold period."
    ),
    global_option!(
        "cmdenv-fake-gui-on-simtime-numsteps",
        String,
        "Number of refreshDisplay() calls when simulation time advances."
    ),
    global_option!(
        "cmdenv-fake-gui-on-simtime-probability",
        Double,
        "Probability of refreshDisplay() calls when simulation time advances."
    ),
    global_option!(
        "cmdenv-fake-gui-seed",
        Int,
        "Seed for the RNG used by fake GUI operations."
    ),
    global_option!(
        "cmdenv-interactive",
        Bool,
        "Prompt for unassigned parameters instead of raising an error."
    ),
    global_option!(
        "cmdenv-log-cleanup",
        Bool,
        "Whether to enable logging during network cleanup."
    ),
    global_option!(
        "cmdenv-log-finalization",
        Bool,
        "Whether to enable logging during network finalization (default depends on express mode)."
    ),
    global_option!(
        "cmdenv-log-initialization",
        Bool,
        "Whether to enable logging during network initialization (default depends on express mode)."
    ),
    global_option!(
        "cmdenv-log-simulation",
        Bool,
        "Whether to enable logging during event processing (default depends on express mode)."
    ),
    global_option!(
        "cmdenv-log-prefix",
        String,
        "Format string for the log line prefix."
    ),
    global_option!(
        "cmdenv-output-file",
        String,
        "File to redirect stdout into."
    ),
    global_option!(
        "cmdenv-performance-display",
        Bool,
        "Print detailed performance information in express mode."
    ),
    global_option!(
        "cmdenv-progress-updates",
        Bool,
        "Print a periodic status update on the progress of the simulation (express mode)."
    ),
    global_option!(
        "cmdenv-redirect-output",
        Bool,
        "Redirect simulation output to a per-run file."
    ),
    global_option!(
        "cmdenv-runs-to-execute",
        String,
        "Filter for which runs to execute (run numbers or filter expressions)."
    ),
    global_option!(
        "cmdenv-status-frequency",
        Quantity("s"),
        "Interval between status updates in express mode."
    ),
    global_option!(
        "cmdenv-stop-batch-on-error",
        Bool,
        "Stop executing remaining runs after an error."
    ),
    // qtenv
    global_option!(
        "qtenv-default-config",
        String,
        "Configuration section Qtenv should set up on startup."
    ),
    global_option!(
        "qtenv-default-run",
        String,
        "Run Qtenv should set up on startup."
    ),
    global_option!(
        "qtenv-extra-stack",
        Quantity("B"),
        "Extra stack space reserved for each activity() module under Qtenv."
    ),
    global_option!(
        "qtenv-identicon-seed",
        String,
        "Seed for generating the identicon shown in the Qtenv toolbar."
    ),
    // eventlog
    global_option!(
        "record-eventlog",
        Bool,
        "Record an eventlog file for the Sequence Chart tool."
    ),
    global_option!(
        "eventlog-file",
        String,
        "Name of the eventlog file to generate."
    ),
    global_option!(
        "eventlog-index-frequency",
        Quantity("B"),
        "How often index data is written into the eventlog file."
    ),
    global_option!(
        "eventlog-max-size",
        Quantity("B"),
        "Maximum eventlog file size before truncation."
    ),
    global_option!(
        "eventlog-message-detail-pattern",
        String,
        "Patterns selecting which message fields to record in detail."
    ),
    global_option!(
        "eventlog-min-truncated-size",
        Quantity("B"),
        "Minimum size the eventlog is truncated to."
    ),
    global_option!(
        "eventlog-options",
        String,
        "Categories of events to record into the eventlog."
    ),
    global_option!(
        "eventlog-recording-intervals",
        String,
        "Simulation time intervals in which to record events."
    ),
    global_option!(
        "eventlog-snapshot-frequency",
        Quantity("B"),
        "How often snapshots are written into the eventlog file."
    ),
    // result recording
    global_option!(
        "config-recording",
        String,
        "Which configuration options to save into result files."
    ),
    global_option!(
        "record-scalar-results",
        Bool,
        "Master switch for recording scalar results."
    ),
    global_option!("output-scalar-file", String, "Output scalar file name."),
    global_option!(
        "output-scalar-file-append",
        Bool,
        "Append to an existing scalar file instead of overwriting."
    ),
    global_option!(
        "output-scalar-precision",
        Int,
        "Significant digits for scalar output."
    ),
    global_option!(
        "output-scalar-db-commit-freq",
        Int,
        "SQLite commit frequency for scalar output."
    ),
    global_option!(
        "record-vector-results",
        Bool,
        "Master switch for recording vector results."
    ),
    global_option!("output-vector-file", String, "Output vector file name."),
    global_option!(
        "output-vector-file-append",
        Bool,
        "Append to an existing vector file instead of overwriting."
    ),
    global_option!(
        "output-vector-precision",
        Int,
        "Significant digits for vector output."
    ),
    global_option!(
        "output-vector-db-indexing",
        String,
        "When to create SQLite indices for vector data."
    ),
    global_option!(
        "output-vectors-memory-limit",
        Quantity("B"),
        "Total memory for buffering output vectors."
    ),
    global_option!("snapshot-file", String, "Name of the snapshot file."),
    // fingerprints
    global_option!(
        "fingerprint",
        String,
        "Expected fingerprint(s) for regression testing."
    ),
    global_option!(
        "fingerprint-events",
        String,
        "Pattern for events included in fingerprint calculation."
    ),
    global_option!(
        "fingerprint-ingredients",
        String,
        "Ingredients making up the fingerprint (e.g. tplx)."
    ),
    global_option!(
        "fingerprint-modules",
        String,
        "Module path pattern for the fingerprint filter."
    ),
    global_option!(
        "fingerprint-results",
        String,
        "Result path pattern for the fingerprint filter."
    ),
    // parallel simulation
    global_option!(
        "parallel-simulation",
        Bool,
        "Enable parallel distributed simulation."
    ),
    global_option!(
        "parsim-num-partitions",
        Int,
        "Number of partitions for parallel simulation."
    ),
    global_option!(
        "parsim-debug",
        Bool,
        "Log parallel simulation debug information."
    ),
    global_option!(
        "parsim-filecommunications-prefix",
        String,
        "File prefix for cross-partition messages."
    ),
    global_option!(
        "parsim-filecommunications-preserve-read",
        Bool,
        "Preserve consumed message files instead of deleting them."
    ),
    global_option!(
        "parsim-filecommunications-read-prefix",
        String,
        "Directory prefix for consumed message files."
    ),
    global_option!(
        "parsim-idealsimulationprotocol-tablesize",
        Int,
        "Ideal Simulation Protocol event trace buffer size."
    ),
    global_option!(
        "parsim-mpicommunications-mpibuffer",
        Int,
        "MPI communications buffer size."
    ),
    global_option!(
        "parsim-namedpipecommunications-prefix",
        String,
        "File system prefix for named pipes."
    ),
    global_option!(
        "parsim-nullmessageprotocol-laziness",
        Double,
        "Null message sending delay parameter."
    ),
    global_option!(
        "parsim-nullmessageprotocol-lookahead-class",
        String,
        "Lookahead calculation class for the Null Message Protocol."
    ),
    global_option!(
        "total-stack",
        Quantity("B"),
        "Maximum memory for activity() module stacks."
    ),
    // class overrides
    global_option!("scheduler-class", String, "Event scheduler class."),
    global_option!(
        "parsim-communications-class",
        String,
        "Partition communications class."
    ),
    global_option!(
        "parsim-synchronization-class",
        String,
        "Parallel simulation synchronization algorithm class."
    ),
    global_option!("eventlogmanager-class", String, "Eventlog manager class."),
    global_option!(
        "outputvectormanager-class",
        String,
        "Output vector manager class."
    ),
    global_option!(
        "outputscalarmanager-class",
        String,
        "Output scalar manager class."
    ),
    global_option!("snapshotmanager-class", String, "Snapshot manager class."),
    global_option!(
        "futureeventset-class",
        String,
        "Future event set (FES) class."
    ),
    global_option!(
        "fingerprintcalculator-class",
        String,
        "Fingerprint calculator class."
    ),
    global_option!(
        "configuration-class",
        String,
        "Configuration information provider class."
    ),
    global_option!(
        "sectionbasedconfig-configreader-class",
        String,
        "Configuration reader class for ini files."
    ),
    global_option!(
        "user-interface",
        String,
        "User interface to start (Cmdenv or Qtenv)."
    ),
];

// Configuration options that can be set on an object
pub static PER_OBJECT_CONFIG: &[BuiltinConfigOption] = &[
    per_object_option!(
        "scalar-recording",
        Bool,
        "Whether to record scalar results."
    ),
    per_object_option!(
        "vector-recording",
        Bool,
        "Whether to record vector results."
    ),
    per_object_option!(
        "vector-recording-intervals",
        String,
        "Time intervals for vector recording."
    ),
    per_object_option!(
        "vector-record-eventnumbers",
        Bool,
        "Record event numbers in vectors."
    ),
    per_object_option!(
        "vector-record-empty",
        Bool,
        "Whether to record output vectors that contain no data."
    ),
    per_object_option!(
        "vector-buffer",
        Quantity("B"),
        "Per-vector buffer space before writing to file."
    ),
    per_object_option!("bin-recording", Bool, "Whether to record histogram bins."),
    per_object_option!(
        "param-recording",
        Bool,
        "Whether to record module parameters."
    ),
    per_object_option!(
        "param-record-as-scalar",
        Bool,
        "Record the matching parameters into the scalar file."
    ),
    per_object_option!(
        "statistic-recording",
        Bool,
        "Enable recording from matching @statistic properties."
    ),
    per_object_option!(
        "result-recording-modes",
        String,
        "Recording modes for @statistic."
    ),
    per_object_option!(
        "record-eventnumbers",
        Bool,
        "Record event numbers in output vectors."
    ),
    per_object_option!(
        "module-eventlog-recording",
        Bool,
        "Enable eventlog recording for the matching modules."
    ),
    per_object_option!(
        "partition-id",
        String,
        "Partition(s) the matching modules are placed into (parallel simulation)."
    ),
    per_object_option!("cmdenv-log-level", String, "Per-module log level."),
    per_object_option!("display-name", String, "Display name for the module."),
    per_object_option!(
        "display-string",
        String,
        "Display string fragment merged into the module's @display property."
    ),
];

// TODO:
/// just a static list of default channel parameters → since I have a ned file for the build in
/// types, this should be later resolved directly from the ned file instead of hardcoding it here
pub static CHANNEL_PARAMS: &[BuiltinConfigOption] = &[
    global_option!("delay", Quantity("s"), "Propagation delay of the channel."),
    global_option!("datarate", Quantity("bps"), "Data rate of the channel."),
    global_option!("ber", Double, "Bit error rate of the channel."),
    global_option!("per", Double, "Packet error rate of the channel."),
    global_option!("disabled", Bool, "Whether the channel is disabled."),
];

/// Returns the BuiltinConfigOption for a channel parameter name
pub fn lookup_channel_param(name: &str) -> Option<&'static BuiltinConfigOption> {
    CHANNEL_PARAMS.iter().find(|c| c.name == name)
}

/// Returns the BuiltinConfigOption for a global option name
pub fn lookup_global(name: &str) -> Option<&'static BuiltinConfigOption> {
    GLOBAL_CONFIG
        .iter()
        .find(|c| c.name == name)
        .or_else(|| lookup_seed_option(name))
}

pub static SEED_OPTIONS: &[BuiltinConfigOption] = &[
    global_option!(
        "seed-N-mt",
        Int,
        "Seed for RNG number N when Mersenne Twister is the RNG class (default). N ranges from 0 to num-rngs-1; unset seeds are derived automatically from seed-set."
    ),
    global_option!(
        "seed-N-lcg32",
        Int,
        "Seed for RNG number N when cLCG32 is the RNG class. Valid range: 1..2^31-2."
    ),
    global_option!(
        "seed-N-mt-pM",
        Int,
        "Parallel simulation: Mersenne Twister seed for RNG number N in partition M."
    ),
];

/// check if non empty and ascii string of numbers
fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Returns the BuiltinConfigOption for a seed option name
fn lookup_seed_option(name: &str) -> Option<&'static BuiltinConfigOption> {
    let rest = name.strip_prefix("seed-")?; // just stip the prefix 
    if let Some(k) = rest.strip_suffix("-mt") {
        return is_number(k).then_some(&SEED_OPTIONS[0]);
    }
    if let Some(k) = rest.strip_suffix("-lcg32") {
        return is_number(k).then_some(&SEED_OPTIONS[1]);
    }
    if let Some((k, p)) = rest.split_once("-mt-p") {
        return (is_number(k) && is_number(p)).then_some(&SEED_OPTIONS[2]);
    }
    None
}

/// Returns the RNG index N of a seed option name (seed-12-mt → 12)
// INFO: I might remove this feature later, since it is not really needed and opp itself simply
// ignores it
pub fn seed_rng_index(name: &str) -> Option<u32> {
    lookup_seed_option(name)?;
    name.strip_prefix("seed-")?.split('-').next()?.parse().ok()
}

static RNG_OPTION: BuiltinConfigOption = per_object_option!(
    "rng-N",
    Int,
    "Maps module-local RNG N to physical RNG index."
);

/// Returns true if the name is a valid RNG mapping option.
fn is_rng_mapping(name: &str) -> bool {
    name.starts_with("rng-") && name[4..].parse::<u32>().is_ok()
}

/// Returns the BuiltinConfigOption for a per object option name.
pub fn lookup_per_object(name: &str) -> Option<&'static BuiltinConfigOption> {
    PER_OBJECT_CONFIG
        .iter()
        .find(|c| c.name == name)
        .or_else(|| {
            if is_rng_mapping(name) {
                Some(&RNG_OPTION)
            } else {
                None
            }
        })
}

#[allow(dead_code)]
pub fn is_builtin(name: &str) -> bool {
    lookup_global(name).is_some() || lookup_per_object(name).is_some()
}

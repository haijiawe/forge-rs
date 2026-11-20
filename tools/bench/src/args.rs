//! Hand-written flag parser for the five oracle flags.
//!
//! Go's `flag` package accepts `-flag value`, `-flag=value`, `--flag`, and
//! stops at the first positional argument. No CLI framework is needed for
//! five options; using one would be a new third-party dependency.

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub out: String,
    pub baseline: String,
    pub fail_over: f64,
    pub strict: bool,
    pub require_baseline: bool,
    pub help: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            out: String::new(),
            baseline: "scripts/bench/baseline.json".to_string(),
            fail_over: 10.0,
            strict: false,
            require_baseline: false,
            help: false,
        }
    }
}

pub fn usage() -> String {
    "usage: hearth-bench [flags]\n\
       \n\
       Runs six metrics and prints p50/p95, the fastest-phase value and its tolerance.\n\
       \n\
       Flags (Go oracle compatible):\n\
       \x20 -out <path>          merge this run into the baseline document at <path>\n\
       \x20 -baseline <path>     compare against <path> (default scripts/bench/baseline.json)\n\
       \x20                      pass \"\" to record without comparison\n\
       \x20 -fail-over <pct>     fallback tolerance when a row has none (default 10)\n\
       \x20 -strict              exit 1 when a metric regresses; skips still exit 0\n\
       \x20 -require-baseline    exit 1 when this platform has no record\n\
       \x20 -h, -help            show this help\n"
        .to_string()
}

/// Parse flags, returning a descriptive error for an unknown flag or a bad
/// numeric/boolean value.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with('-') || arg == "-" {
            break; // Go flag stops at the first positional argument.
        }
        let trimmed = arg.trim_start_matches('-');
        if trimmed.is_empty() {
            return Err(format!("bench: bad flag: {arg}"));
        }
        let (name, inline) = match trimmed.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (trimmed, None),
        };
        match name {
            "h" | "help" => {
                opts.help = true;
                if let Some(value) = inline {
                    if !parse_bool(&value)? {
                        opts.help = false;
                    }
                }
            }
            "out" | "baseline" | "fail-over" => {
                let value = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        args.get(i)
                            .cloned()
                            .ok_or_else(|| format!("bench: -{name} needs a value"))?
                    }
                };
                match name {
                    "out" => opts.out = value,
                    "baseline" => opts.baseline = value,
                    _ => {
                        opts.fail_over = value
                            .parse::<f64>()
                            .map_err(|e| format!("bench: bad -fail-over value {value:?}: {e}"))?;
                        if !opts.fail_over.is_finite() || opts.fail_over < 0.0 {
                            return Err(format!(
                                "bench: -fail-over must be a non-negative number, got {value:?}"
                            ));
                        }
                    }
                }
            }
            "strict" | "require-baseline" => {
                let value = match inline {
                    Some(v) => parse_bool(&v)?,
                    None => true,
                };
                if name == "strict" {
                    opts.strict = value;
                } else {
                    opts.require_baseline = value;
                }
            }
            _ => return Err(format!("bench: unknown flag -{name}")),
        }
        i += 1;
    }
    Ok(opts)
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Ok(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Ok(false),
        _ => Err(format!("bench: bad boolean value {value:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn defaults_match_the_go_oracle() {
        let opts = parse_args(&[]).unwrap();
        assert_eq!(opts, Options::default());
        assert_eq!(opts.baseline, "scripts/bench/baseline.json");
        assert_eq!(opts.fail_over, 10.0);
    }

    #[test]
    fn parses_single_dash_double_dash_and_equals_forms() {
        let opts = parse_args(&args(&[
            "-out",
            "scripts/bench/baseline.json",
            "--baseline=",
            "-fail-over=7.5",
            "-strict",
            "--require-baseline=false",
        ]))
        .unwrap();
        assert_eq!(opts.out, "scripts/bench/baseline.json");
        assert_eq!(opts.baseline, "");
        assert_eq!(opts.fail_over, 7.5);
        assert!(opts.strict);
        assert!(!opts.require_baseline);
    }

    #[test]
    fn stops_at_first_positional_argument() {
        let opts = parse_args(&args(&["-strict", "extra", "-require-baseline"])).unwrap();
        assert!(opts.strict);
        assert!(!opts.require_baseline);
    }

    #[test]
    fn rejects_unknown_flags_and_bad_values() {
        assert!(parse_args(&args(&["-nope"]))
            .unwrap_err()
            .contains("unknown flag"));
        assert!(parse_args(&args(&["-fail-over"]))
            .unwrap_err()
            .contains("needs a value"));
        assert!(parse_args(&args(&["-fail-over=abc"]))
            .unwrap_err()
            .contains("bad -fail-over"));
        assert!(parse_args(&args(&["-fail-over=-1"]))
            .unwrap_err()
            .contains("non-negative"));
        assert!(parse_args(&args(&["-strict=maybe"]))
            .unwrap_err()
            .contains("bad boolean"));
        assert!(parse_args(&args(&["-help"])).unwrap().help);
        assert!(!usage().is_empty());
    }
}

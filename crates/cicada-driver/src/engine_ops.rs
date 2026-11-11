//! Engine ops — `cli.<verb>` wire queries dispatch into the
//! `cicada` engine lib through this binary's own `run` verb: each
//! call re-execs `tamias-driver-cicada run <argv>` with the
//! request's params, so `cicada::dispatch` writes to the child's
//! captured stdout and never touches the serve loop's ndjson
//! stream — and every mutation keeps the engine's own semantics
//! (manifest discovery, confirmation gate, state journal).
//!
//! `ops` answers the ops manifest as JSON — *derived from the
//! engine's own clap tree* ([`cicada::command`]), so a verb can't
//! exist on the CLI and be missing here: declared args become
//! typed params (`arg`/`opt`/`flag`), doc strings ride along
//! unchanged.
//!
//! cicada has one verb that can never be a request/response op —
//! it rides `surfaces` (argv passthrough / generated-surface
//! subcommands with inherited stdio) instead:
//!
//! - `provider.add` — the OAuth/service-account credential flow:
//!   it reads the auth code from stdin or waits on a loopback
//!   listener plus a browser prompt. A wire op child
//!   (`Command::output`) has no usable stdin/tty, so the
//!   interactive path can only ever fail as an op.
//!
//! (The old `mcp` surface is gone with cicada-mcp — T-019 moved the
//! preview→digest→apply gate engine-side: `plan` issues a
//! `planDigest` persisted beside the state file and `apply
//! --plan-digest` enforces it, so the gate rides ordinary `cli.*`
//! ops. The driver's own `mcp` verb — feature `mcp`, on by default —
//! renders this ops manifest as MCP tools, the agent surface.)
//!
//! `mutates` is honest: `init` scaffolds files, `plan` can write
//! `--out`, `apply`/`refresh`/`fmt` rewrite state, remote events,
//! or calendar YAML. `validate`/`drift`/`range`/`show` only read.
//! `apply` without `--auto-approve` prompts on stdin — a wire op
//! child's stdin is null, so `confirm` fails closed (abort). No
//! streaming *flags* exist; secret-ish params (`--client-secret`,
//! `--code`) live only on the `provider.add` surface, never in
//! ops — and even there they carry names, never values, in the
//! manifest.

use clap::{Arg, ArgAction, Command};
use pavo_protocol::wire::{QueryResult, Response};
use serde_json::{Map, Value, json};

/// Unbounded/host-interactive surfaces — verbs that need inherited
/// stdin/tty/browser (`provider.add`, see module docs). They can
/// never be request/response ops. (cicada-mcp's `mcp` verb was
/// deleted in T-019 — the digest gate moved engine-side.)
const SURFACES: &[&str] = &["provider.add"];

/// Params excluded from ops — flags that turn a bounded verb into
/// a stream, or params that must never ride argv. None today:
/// cicada takes no secret *values* on bounded verbs (OAuth
/// secrets live on the `provider.add` surface).
const SKIP_PARAMS: &[&str] = &[];

/// Verbs with side effects — file scaffolding (`init`), remote
/// reconciliation (`apply`), recorded-state writes (`refresh`),
/// in-place YAML rewrites (`fmt`), and `plan`'s optional `--out`
/// write. Mutators belong behind a deliberate trigger, not
/// ambient view polling.
const MUTATES: &[&str] = &["init", "plan", "apply", "refresh", "fmt"];

/// `(name, clap spec)` for every leaf verb. Hidden plumbing and
/// clap's `help` are skipped — they stay reachable through `run`
/// passthrough.
fn verbs() -> Vec<(String, Command)> {
    fn walk(prefix: &str, cmd: &Command, out: &mut Vec<(String, Command)>) {
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            if name == "help" || sub.is_hide_set() {
                continue;
            }
            let dotted = match prefix {
                "" => name.to_string(),
                _ => format!("{prefix}.{name}"),
            };
            if sub.has_subcommands() {
                walk(&dotted, sub, out);
            } else {
                out.push((dotted, sub.clone()));
            }
        }
    }
    let mut out = Vec::new();
    walk("", &cicada::command(), &mut out);
    out
}

fn op_cmd(name: &str) -> Option<Command> {
    verbs()
        .into_iter()
        .find(|(n, _)| n == name && !SURFACES.contains(&n.as_str()))
        .map(|(_, c)| c)
}

/// Args that can't ride the wire: streaming flags (above) and
/// clap's own help/version actions. cicada has no driver-resolved
/// global — verbs find `cicada.yaml` by walking up from the
/// child's inherited cwd, same as running `cicada` by hand.
fn skipped(a: &Arg) -> bool {
    SKIP_PARAMS.contains(&a.get_id().as_str())
        || matches!(
            a.get_action(),
            ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
        )
}

/// A clap arg → the manifest's typed param kind — `arg` =
/// positional, `opt` = `--name <v>`, `flag` = `--name` — the
/// contract `pavo-driver-kit`'s surface layer renders args from.
fn kind(a: &Arg) -> &'static str {
    if a.is_positional() {
        "arg"
    } else if matches!(
        a.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
    ) {
        "flag"
    } else {
        "opt"
    }
}

fn param_json(a: &Arg) -> Value {
    // Wire name = the flag name (`--fake-file` → `fake-file`),
    // not the field id (`fake_file`) — generated surfaces spell
    // params `--<name>`.
    let id = a.get_id().as_str();
    let name = if a.is_positional() {
        id
    } else {
        a.get_long().unwrap_or(id)
    };
    json!({
        "name": name,
        "kind": kind(a),
        "doc": a.get_help().map(ToString::to_string).unwrap_or_default(),
    })
}

/// The `ops` query/`ops` argv verb payload — self-describing so
/// hosts and agents discover the engine surface without argv
/// help. cicada has no globals: every verb resolves its manifest,
/// state, and provider from args/env (`CICADA_STATE`,
/// `CICADA_GOOGLE_*`) and the child's inherited cwd — listed once
/// here for discoverability.
pub fn manifest() -> Value {
    json!({
        "engine": "cicada",
        "invoke": "query `cli.<verb>` with the listed params, or argv `tamias-driver-cicada run <cicada argv>`",
        "note": "ops inherit the serve process's cwd — `cicada.yaml` manifest discovery, `.cicada/` state, and `CICADA_*` env apply exactly as for the `cicada` binary.",
        "globals": [],
        "ops": verbs().iter()
            .filter(|(n, _)| !SURFACES.contains(&n.as_str()))
            .map(|(n, c)| json!({
                "query": format!("cli.{n}"),
                "doc": c.get_about().map(ToString::to_string).unwrap_or_default(),
                "params": c.get_arguments()
                    .filter(|a| !skipped(a))
                    .map(param_json)
                    .collect::<Vec<_>>(),
                "mutates": MUTATES.contains(&n.as_str()),
            }))
            .collect::<Vec<_>>(),
        "surfaces": SURFACES,
    })
}

/// Is `query` an engine op (`cli.<verb>`)? Returns the verb name.
pub fn op_of(query: &str) -> Option<String> {
    query
        .strip_prefix("cli.")
        .filter(|n| op_cmd(n).is_some())
        .map(str::to_string)
}

/// A wire scalar → its argv token (numbers/bools stringify).
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// All values of a param — arrays flatten, scalars wrap.
fn values(v: &Value) -> Vec<String> {
    match v {
        Value::Array(xs) => xs.iter().filter_map(scalar).collect(),
        other => scalar(other).into_iter().collect(),
    }
}

/// `cli.<verb>` params → cicada argv (no argv[0] — `run` prepends
/// it). cicada has no globals, so the param loop covers everything:
/// declared params map by kind — positionals land last, in schema
/// order. Dotted names (`provider.add`-style) split into nested
/// subcommands should a future verb nest.
fn argv(name: &str, cmd: &Command, params: &Map<String, Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    out.extend(name.split('.').map(String::from));
    for a in cmd.get_arguments() {
        if skipped(a) || a.is_positional() {
            continue;
        }
        let id = a.get_id().as_str();
        let long = a.get_long().unwrap_or(id);
        let raw = params
            .get(long)
            .or_else(|| params.get(id))
            .or_else(|| params.get(id.replace('_', "-").as_str()));
        let Some(raw) = raw else { continue };
        match kind(a) {
            "flag" => {
                if raw.as_bool().unwrap_or(false) {
                    out.push(format!("--{long}"));
                }
            }
            _ => {
                for s in values(raw) {
                    out.push(format!("--{long}"));
                    out.push(s);
                }
            }
        }
    }
    // `trailing_var_arg` positionals only parse after a `--`
    // separator — none today, but the separator logic stays
    // generic for any future last-positional.
    if cmd.get_positionals().any(Arg::is_last_set) {
        out.push("--".into());
    }
    let mut positionals: Vec<String> = Vec::new();
    for a in cmd.get_positionals() {
        if skipped(a) {
            continue;
        }
        let id = a.get_id().as_str();
        if let Some(raw) = params
            .get(id)
            .or_else(|| params.get(id.replace('_', "-").as_str()))
        {
            positionals.extend(values(raw));
        }
    }
    out.extend(positionals);
    out
}

/// The bare-name entry the embedded `mcp` surface uses (`OpSpec.name`
/// is `cli.`-stripped); the wire path goes `op_of` → `run_verb`.
#[cfg(feature = "mcp")]
pub fn run_op(op: &str, params: &Map<String, Value>) -> Response {
    match op_of(&format!("cli.{}", op.strip_prefix("cli.").unwrap_or(op))) {
        Some(name) => run_verb(&name, params),
        None => Response::err(format!("unknown op: {op}")),
    }
}

/// Run one engine op by name: re-exec `run` in a child with
/// captured output, and hand back `{op, argv, code, stdout,
/// stderr}` as the query payload. `ok:false` is reserved for spawn
/// failures — a nonzero `code` is the engine's own exit status
/// (e.g. `drift` findings, a failed `apply`), honest data. The
/// child's stdin is null and it inherits this process's cwd —
/// `cicada.yaml` discovery, `.cicada/` state, and `CICADA_*` env
/// apply exactly as for the `cicada` binary; host-interactive
/// verbs (`mcp`, `provider.add`) are surfaces, never ops,
/// precisely because they need inherited stdio.
pub fn run_verb(name: &str, params: &Map<String, Value>) -> Response {
    let Some(cmd) = op_cmd(name) else {
        return Response::err(format!("unknown op: {name}"));
    };
    let args = argv(name, &cmd, params);
    let out = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .arg("run")
            .args(&args)
            .output()
    });
    let data = match out {
        Ok(o) => json!({
            "op": name,
            "argv": args,
            "code": o.status.code().unwrap_or(-1),
            "stdout": String::from_utf8_lossy(&o.stdout),
            "stderr": String::from_utf8_lossy(&o.stderr),
        }),
        Err(e) => return Response::err(format!("run {name}: {e}")),
    };
    Response::ok_typed(&QueryResult {
        query: format!("cli.{name}"),
        data,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn op_of_accepts_cli_prefix_only() {
        assert_eq!(op_of("cli.validate").unwrap(), "validate");
        assert_eq!(op_of("cli.plan").unwrap(), "plan");
        assert!(op_of("cli.mcp").is_none()); // deleted with cicada-mcp (T-019)
        assert!(op_of("cli.provider.add").is_none()); // interactive surface
        assert!(op_of("cli.provider").is_none()); // group, not a leaf
        assert!(op_of("validate").is_none()); // view query, not an op
        assert!(op_of("cli.bogus").is_none());
    }

    #[test]
    fn manifest_lists_every_non_surface_verb_as_cli_query() {
        let m = manifest();
        let ops = m["ops"].as_array().unwrap();
        for (n, _) in verbs()
            .iter()
            .filter(|(n, _)| !SURFACES.contains(&n.as_str()))
        {
            assert!(
                ops.iter().any(|o| o["query"] == json!(format!("cli.{n}"))),
                "{n} missing from ops manifest"
            );
        }
        assert_eq!(m["surfaces"], json!(SURFACES));
        for o in ops {
            for p in o["params"].as_array().unwrap() {
                assert!(["arg", "opt", "flag"].contains(&p["kind"].as_str().unwrap()));
                // streaming/stdin params never ride the wire
                assert!(!SKIP_PARAMS.contains(&p["name"].as_str().unwrap()));
            }
        }
        // mutates honesty — the side-effect set is exactly these
        let mutators: Vec<&str> = ops
            .iter()
            .filter(|o| o["mutates"] == json!(true))
            .filter_map(|o| o["query"].as_str())
            .collect();
        assert_eq!(
            mutators,
            vec![
                "cli.init",
                "cli.plan",
                "cli.apply",
                "cli.refresh",
                "cli.fmt"
            ]
        );
    }

    #[test]
    fn argv_maps_params_to_cicada_flags() {
        let cmd = op_cmd("apply").unwrap();
        let mut p = Map::new();
        p.insert("auto-approve".into(), json!(true));
        p.insert("resume".into(), json!(true));
        p.insert("provider".into(), json!("fake"));
        assert_eq!(
            argv("apply", &cmd, &p),
            vec!["apply", "--provider", "fake", "--auto-approve", "--resume"]
        );
    }

    #[test]
    fn argv_places_positionals_last() {
        let cmd = op_cmd("show").unwrap();
        let mut p = Map::new();
        p.insert("id".into(), json!("standup"));
        p.insert("files".into(), json!(["calendars/work.yaml"]));
        p.insert("from".into(), json!("+7d"));
        assert_eq!(
            argv("show", &cmd, &p),
            vec!["show", "--from", "+7d", "standup", "calendars/work.yaml"]
        );
    }

    #[test]
    fn argv_omits_absent_params() {
        let cmd = op_cmd("drift").unwrap();
        let mut p = Map::new();
        p.insert("provider".into(), json!("fake"));
        assert_eq!(argv("drift", &cmd, &p), vec!["drift", "--provider", "fake"]);
    }
}

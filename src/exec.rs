//! User commands: placeholder expansion, spawning, confirmation and output handling.
//!
//! Placeholders describe the current image (`{file} {dir} {name} {stem} {ext} {index} {width}
//! {height}`) and the targets (`{files} {count}`). In a shell string (`sh -c`) every value is
//! shell-quoted; in an exec array an element containing `{files}` repeats once per target.
//! Anything else in braces is left alone, so `awk '{print $1}'` survives.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use crate::config::{Command, CommandLine, Output, Stdin, Targets};

/// What a command can see of the viewer when it starts.
#[derive(Clone, Debug, Default)]
pub struct Context<'a> {
    pub current: Option<&'a Path>,
    /// 0-based position of `current` in the queue.
    pub index: usize,
    pub targets: &'a [PathBuf],
    /// Pixel size of the current image, when known.
    pub size: Option<[u32; 2]>,
    pub mode: &'a str,
    pub socket: Option<&'a Path>,
}

/// A command ready to spawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Prepared {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(&'static str, OsString)>,
    pub stdin: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Var {
    File,
    Files,
    Dir,
    Name,
    Stem,
    Ext,
    Index,
    Count,
    Width,
    Height,
}

const VARS: &[(&str, Var)] = &[
    ("file", Var::File),
    ("files", Var::Files),
    ("dir", Var::Dir),
    ("name", Var::Name),
    ("stem", Var::Stem),
    ("ext", Var::Ext),
    ("index", Var::Index),
    ("count", Var::Count),
    ("width", Var::Width),
    ("height", Var::Height),
];

enum Piece<'t> {
    Lit(&'t str),
    Var(Var),
}

/// Split a template into literal text and known placeholders.
fn pieces(template: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let var = rest[open + 1..].find('}').and_then(|close| {
            let name = &rest[open + 1..open + 1 + close];
            VARS.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| (*v, open + close + 2))
        });
        match var {
            Some((v, end)) => {
                if open > 0 {
                    out.push(Piece::Lit(&rest[..open]));
                }
                out.push(Piece::Var(v));
                rest = &rest[end..];
            }
            None => {
                out.push(Piece::Lit(&rest[..=open]));
                rest = &rest[open + 1..];
            }
        }
    }
    if !rest.is_empty() {
        out.push(Piece::Lit(rest));
    }
    out
}

fn mentions(template: &str, f: impl Fn(Var) -> bool) -> bool {
    pieces(template)
        .iter()
        .any(|p| matches!(p, Piece::Var(v) if f(*v)))
}

/// The value of a single-valued placeholder.
fn value(v: Var, ctx: &Context) -> OsString {
    let cur = ctx.current;
    let os = |s: Option<&OsStr>| s.map(OsStr::to_os_string).unwrap_or_default();
    match v {
        Var::File => os(cur.map(Path::as_os_str)),
        Var::Dir => os(cur.and_then(Path::parent).map(Path::as_os_str)),
        Var::Name => os(cur.and_then(Path::file_name)),
        Var::Stem => os(cur.and_then(Path::file_stem)),
        Var::Ext => os(cur.and_then(Path::extension)),
        Var::Index if cur.is_some() => (ctx.index + 1).to_string().into(),
        Var::Count => ctx.targets.len().to_string().into(),
        Var::Width => ctx
            .size
            .map(|s| s[0].to_string())
            .unwrap_or_default()
            .into(),
        Var::Height => ctx
            .size
            .map(|s| s[1].to_string())
            .unwrap_or_default()
            .into(),
        Var::Index | Var::Files => OsString::new(),
    }
}

/// Quote for `sh`: safe words stay bare, everything else goes in single quotes.
pub fn shell_quote(s: &OsStr) -> Vec<u8> {
    let b = s.as_bytes();
    let safe = |c: &u8| c.is_ascii_alphanumeric() || b"_-./,:@%+=".contains(c);
    if !b.is_empty() && b.iter().all(safe) {
        return b.to_vec();
    }
    let mut out = Vec::with_capacity(b.len() + 2);
    out.push(b'\'');
    for &c in b {
        if c == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(c);
        }
    }
    out.push(b'\'');
    out
}

/// A shell script with every placeholder quoted; `{files}` becomes the quoted targets.
fn expand_shell(template: &str, ctx: &Context) -> OsString {
    let mut out = Vec::new();
    for p in pieces(template) {
        match p {
            Piece::Lit(s) => out.extend_from_slice(s.as_bytes()),
            Piece::Var(Var::Files) => {
                for (i, f) in ctx.targets.iter().enumerate() {
                    if i > 0 {
                        out.push(b' ');
                    }
                    out.extend(shell_quote(f.as_os_str()));
                }
            }
            Piece::Var(v) => out.extend(shell_quote(&value(v, ctx))),
        }
    }
    OsString::from_vec(out)
}

/// One exec argument; with `{files}` in it, one argument per target.
fn expand_arg(template: &str, ctx: &Context) -> Vec<OsString> {
    let pieces = pieces(template);
    let build = |file: Option<&Path>| {
        let mut out = OsString::new();
        for p in &pieces {
            match p {
                Piece::Lit(s) => out.push(s),
                Piece::Var(Var::Files) => out.push(file.map_or(OsStr::new(""), Path::as_os_str)),
                Piece::Var(v) => out.push(value(*v, ctx)),
            }
        }
        out
    };
    if pieces.iter().any(|p| matches!(p, Piece::Var(Var::Files))) {
        ctx.targets.iter().map(|f| build(Some(f))).collect()
    } else {
        vec![build(None)]
    }
}

/// Plain text for the confirm prompt and messages: no quoting, lossy.
pub fn expand_text(template: &str, ctx: &Context) -> String {
    let mut out = String::new();
    for p in pieces(template) {
        match p {
            Piece::Lit(s) => out.push_str(s),
            Piece::Var(Var::Files) => {
                let names: Vec<_> = ctx
                    .targets
                    .iter()
                    .map(|f| crate::ui::file_name(f))
                    .collect();
                out.push_str(&names.join(" "));
            }
            Piece::Var(v) => out.push_str(&value(v, ctx).to_string_lossy()),
        }
    }
    out
}

/// `~/…` at the start of an exec element means the home directory.
fn tilde(s: &str) -> std::borrow::Cow<'_, str> {
    match (s.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => format!("{}/{rest}", home.to_string_lossy()).into(),
        _ => s.into(),
    }
}

/// Whether the command needs at least one file to make sense.
pub fn needs_files(cmd: &Command) -> bool {
    let files = |v: Var| v != Var::Count;
    cmd.stdin != Stdin::None
        || match &cmd.run {
            CommandLine::Shell(s) => mentions(s, files),
            CommandLine::Exec(v) => v.iter().any(|a| mentions(a, files)),
        }
}

/// The files a command acts on. `marked` is in queue order.
pub fn targets(
    t: Targets,
    current: Option<&Path>,
    marked: &[&Path],
    all: &[PathBuf],
) -> Vec<PathBuf> {
    let cur = || current.map(Path::to_path_buf).into_iter().collect();
    let marks = || marked.iter().map(|p| p.to_path_buf()).collect();
    match t {
        Targets::Auto if marked.is_empty() => cur(),
        Targets::Auto | Targets::Marked => marks(),
        Targets::Current => cur(),
        Targets::All => all.to_vec(),
    }
}

pub fn prepare(cmd: &Command, ctx: &Context) -> Prepared {
    let (program, args) = match &cmd.run {
        CommandLine::Shell(s) => (
            OsString::from("sh"),
            vec!["-c".into(), expand_shell(s, ctx)],
        ),
        CommandLine::Exec(v) => {
            let mut all = v.iter().flat_map(|a| expand_arg(&tilde(a), ctx));
            let program = all.next().unwrap_or_default();
            (program, all.collect())
        }
    };
    let sep = match cmd.stdin {
        Stdin::None => None,
        Stdin::Files => Some(b'\n'),
        Stdin::Files0 => Some(0),
    };
    let stdin = sep.map(|sep| {
        let mut buf = Vec::new();
        for f in ctx.targets {
            buf.extend_from_slice(f.as_os_str().as_bytes());
            buf.push(sep);
        }
        buf
    });
    let mut env = vec![
        ("REMAGE_FILE", value(Var::File, ctx)),
        ("REMAGE_INDEX", value(Var::Index, ctx)),
        ("REMAGE_COUNT", value(Var::Count, ctx)),
        ("REMAGE_MODE", ctx.mode.into()),
        ("REMAGE_PID", std::process::id().to_string().into()),
    ];
    if let Some(s) = ctx.socket {
        env.push(("REMAGE_SOCKET", s.as_os_str().to_os_string()));
    }
    Prepared {
        program,
        args,
        env,
        stdin,
    }
}

/// Progress of a running command.
#[derive(Debug)]
pub enum Event {
    /// A stdout line, with `output = "actions"`.
    Line(String),
    Done(Outcome),
}

#[derive(Debug)]
pub struct Outcome {
    /// The exit status, or why the command couldn't start.
    pub status: Result<ExitStatus, String>,
    /// The last non-empty stdout line, with `output = "status"`.
    pub last_line: Option<String>,
    pub first_stderr: Option<String>,
}

impl Outcome {
    pub fn success(&self) -> bool {
        self.status.as_ref().is_ok_and(ExitStatus::success)
    }

    /// `<code> <first stderr line>` for the status line.
    pub fn error(&self) -> String {
        let code = match &self.status {
            Err(e) => return e.clone(),
            Ok(s) => match (s.code(), s.signal()) {
                (Some(c), _) => c.to_string(),
                (None, Some(sig)) => format!("signal {sig}"),
                (None, None) => "failed".into(),
            },
        };
        match &self.first_stderr {
            Some(line) => format!("{code} {line}"),
            None => code,
        }
    }
}

/// Lines are read lossily and capped so a runaway command can't eat memory.
const MAX_LINE: usize = 64 << 10;

fn read_line(r: &mut impl BufRead, buf: &mut Vec<u8>) -> Option<String> {
    buf.clear();
    match r.by_ref().take(MAX_LINE as u64).read_until(b'\n', buf) {
        Ok(0) | Err(_) => None,
        Ok(_) => {
            if buf.last() != Some(&b'\n') && buf.len() == MAX_LINE {
                // Skip the rest of an over-long line.
                let mut sink = Vec::new();
                let _ = r.read_until(b'\n', &mut sink);
            }
            Some(String::from_utf8_lossy(buf).trim_end().to_string())
        }
    }
}

/// Run `p` on its own threads; `sink` gets stdout lines (for `actions`) and then the outcome.
pub fn spawn(name: &str, p: Prepared, output: Output, sink: impl Fn(Event) + Send + 'static) {
    let label = name.to_string();
    let run = move || {
        let mut cmd = std::process::Command::new(&p.program);
        cmd.args(&p.args)
            .envs(p.env.iter().map(|(k, v)| (k, v)))
            .stdin(if p.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(if output == Output::Ignore {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let program = p.program.to_string_lossy();
                return sink(Event::Done(Outcome {
                    status: Err(format!("{program}: {e}")),
                    last_line: None,
                    first_stderr: None,
                }));
            }
        };
        log::debug!("command {label}: pid {}", child.id());
        if let (Some(mut w), Some(data)) = (child.stdin.take(), p.stdin) {
            // Its own thread: a child that writes before reading must not deadlock us.
            std::thread::spawn(move || {
                let _ = w.write_all(&data);
            });
        }
        let stderr = child.stderr.take().map(|e| {
            std::thread::spawn(move || {
                let mut r = BufReader::new(e);
                let mut buf = Vec::new();
                let mut first = None;
                while let Some(line) = read_line(&mut r, &mut buf) {
                    if first.is_none() && !line.trim().is_empty() {
                        first = Some(line.trim().to_string());
                    }
                }
                first
            })
        });
        let mut last_line = None;
        if let Some(out) = child.stdout.take() {
            let mut r = BufReader::new(out);
            let mut buf = Vec::new();
            while let Some(line) = read_line(&mut r, &mut buf) {
                if line.trim().is_empty() {
                    continue;
                }
                match output {
                    Output::Actions => sink(Event::Line(line)),
                    _ => last_line = Some(line.trim().to_string()),
                }
            }
        }
        let status = child.wait().map_err(|e| e.to_string());
        let first_stderr = stderr.and_then(|t| t.join().ok()).flatten();
        sink(Event::Done(Outcome {
            status,
            last_line,
            first_stderr,
        }));
    };
    if let Err(e) = std::thread::Builder::new()
        .name("remage-command".into())
        .spawn(run)
    {
        log::error!("command {name}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ActionList;

    fn cmd(run: CommandLine) -> Command {
        Command {
            desc: String::new(),
            run,
            targets: Targets::Auto,
            stdin: Stdin::None,
            confirm: None,
            output: Output::Status,
            after: ActionList::default(),
        }
    }

    fn ctx<'a>(targets: &'a [PathBuf]) -> Context<'a> {
        Context {
            current: Some(Path::new("/pix/it's a.png")),
            index: 4,
            targets,
            size: Some([640, 480]),
            mode: "image",
            socket: Some(Path::new("/run/remage/1.sock")),
        }
    }

    fn files() -> Vec<PathBuf> {
        vec!["/pix/a b.png".into(), "/pix/c.jpg".into()]
    }

    #[test]
    fn quoting() {
        let q = |s: &str| String::from_utf8(shell_quote(OsStr::new(s))).unwrap();
        assert_eq!(q("/a/b-c_d.png"), "/a/b-c_d.png");
        assert_eq!(q("a b"), "'a b'");
        assert_eq!(q("it's"), "'it'\\''s'");
        assert_eq!(q(""), "''");
        assert_eq!(q("$(rm -rf ~)"), "'$(rm -rf ~)'");
    }

    #[test]
    fn shell_strings_quote_every_placeholder() {
        let f = files();
        let p = prepare(
            &cmd(CommandLine::Shell(
                "cp {files} {dir} && echo {name} {stem}.{ext} {index}/{count} {width}x{height} \
                 | awk '{print $1}' {nope}"
                    .into(),
            )),
            &ctx(&f),
        );
        assert_eq!(p.program, "sh");
        assert_eq!(
            p.args[1],
            "cp '/pix/a b.png' /pix/c.jpg /pix && echo 'it'\\''s a.png' 'it'\\''s a'.png 5/2 \
             640x480 | awk '{print $1}' {nope}"
        );
    }

    #[test]
    fn exec_arrays_expand_files_per_target() {
        let f = files();
        let p = prepare(
            &cmd(CommandLine::Exec(vec![
                "rm".into(),
                "--".into(),
                "{files}".into(),
                "--x={files}".into(),
                "{file}".into(),
            ])),
            &ctx(&f),
        );
        assert_eq!(p.program, "rm");
        let args: Vec<_> = p.args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--",
                "/pix/a b.png",
                "/pix/c.jpg",
                "--x=/pix/a b.png",
                "--x=/pix/c.jpg",
                "/pix/it's a.png"
            ]
        );
        // `~/` expands in the template, never in file names.
        let p = prepare(
            &cmd(CommandLine::Exec(vec!["~/bin/x".into(), "{file}".into()])),
            &Context {
                current: Some(Path::new("~/a.png")),
                ..ctx(&f)
            },
        );
        assert!(!p.program.to_string_lossy().starts_with('~'));
        assert_eq!(p.args[0], "~/a.png");
    }

    #[test]
    fn stdin_and_env() {
        let f = files();
        let mut c = cmd(CommandLine::Exec(vec!["handler".into()]));
        assert!(prepare(&c, &ctx(&f)).stdin.is_none());
        assert!(!needs_files(&c));
        c.stdin = Stdin::Files0;
        let p = prepare(&c, &ctx(&f));
        assert_eq!(p.stdin.as_deref(), Some(&b"/pix/a b.png\0/pix/c.jpg\0"[..]));
        assert!(needs_files(&c));
        let env: std::collections::HashMap<_, _> = p.env.into_iter().collect();
        assert_eq!(env["REMAGE_FILE"], "/pix/it's a.png");
        assert_eq!(env["REMAGE_INDEX"], "5");
        assert_eq!(env["REMAGE_COUNT"], "2");
        assert_eq!(env["REMAGE_MODE"], "image");
        assert_eq!(env["REMAGE_SOCKET"], "/run/remage/1.sock");
        assert!(needs_files(&cmd(CommandLine::Shell("echo {name}".into()))));
        assert!(!needs_files(&cmd(CommandLine::Shell(
            "echo {count}".into()
        ))));
    }

    #[test]
    fn confirm_text() {
        let f = files();
        assert_eq!(
            expand_text("Delete {count} file(s)? {files}", &ctx(&f)),
            "Delete 2 file(s)? a b.png c.jpg"
        );
    }

    #[test]
    fn target_sets() {
        let all = files();
        let cur = Some(Path::new("/pix/c.jpg"));
        let none: [&Path; 0] = [];
        let marked = [Path::new("/pix/a b.png")];
        assert_eq!(targets(Targets::Auto, cur, &none, &all), [all[1].clone()]);
        assert_eq!(targets(Targets::Auto, cur, &marked, &all), [all[0].clone()]);
        assert_eq!(
            targets(Targets::Current, cur, &marked, &all),
            [all[1].clone()]
        );
        assert!(targets(Targets::Marked, cur, &none, &all).is_empty());
        assert_eq!(targets(Targets::All, None, &none, &all), all);
    }

    fn run(run: CommandLine, output: Output, stdin: Option<&[u8]>) -> (Vec<String>, Outcome) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut p = prepare(&cmd(run), &ctx(&[]));
        p.stdin = stdin.map(<[u8]>::to_vec);
        spawn("test", p, output, move |e| {
            let _ = tx.send(e);
        });
        let mut lines = Vec::new();
        loop {
            match rx.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(Event::Line(l)) => lines.push(l),
                Ok(Event::Done(o)) => return (lines, o),
                Err(e) => panic!("{e}"),
            }
        }
    }

    #[test]
    fn spawning_reports_output_and_errors() {
        let (_, o) = run(
            CommandLine::Shell("echo one; echo; echo two; echo $REMAGE_MODE".into()),
            Output::Status,
            None,
        );
        assert!(o.success());
        assert_eq!(o.last_line.as_deref(), Some("image"));

        let (lines, o) = run(
            CommandLine::Shell("cat; echo next".into()),
            Output::Actions,
            Some(b"goto 3\n"),
        );
        assert!(o.success() && o.last_line.is_none());
        assert_eq!(lines, ["goto 3", "next"]);

        let (_, o) = run(
            CommandLine::Shell("echo oops >&2; echo more >&2; exit 3".into()),
            Output::Ignore,
            None,
        );
        assert!(!o.success());
        assert_eq!(o.error(), "3 oops");

        let (_, o) = run(
            CommandLine::Exec(vec!["/nonexistent/remage-test".into()]),
            Output::Status,
            None,
        );
        assert!(
            o.error().starts_with("/nonexistent/remage-test: "),
            "{}",
            o.error()
        );
    }
}

//! Unix-socket server speaking newline-delimited JSON, with event subscriptions, and the
//! `remage msg` client.
//!
//! Requests: `{"action":"goto 5"}` (or an array of actions), `{"query":"current"}` and
//! `{"subscribe":["image","marks","mode"]}`, which turns the connection into an event stream.
//! Replies: `{"ok":true,"data":…}` or `{"ok":false,"error":"…"}`; events:
//! `{"event":"image","data":…}`.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender};
use serde_json::{Value, json};

use crate::action::Action;

/// Longest request line accepted.
const MAX_REQUEST: u64 = 1 << 20;
/// How long a connection waits for the event loop to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    Current,
    Marked,
    Queue,
    State,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Topic {
    Image,
    Marks,
    Mode,
}

impl Topic {
    pub fn name(self) -> &'static str {
        match self {
            Topic::Image => "image",
            Topic::Marks => "marks",
            Topic::Mode => "mode",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Actions(Vec<Action>),
    Query(Query),
    Subscribe(Vec<Topic>),
}

fn word<T: Copy>(what: &str, s: &str, opts: &[(&str, T)]) -> Result<T, String> {
    opts.iter()
        .find(|(k, _)| *k == s)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let names: Vec<_> = opts.iter().map(|(k, _)| *k).collect();
            format!("unknown {what} `{s}`: expected {}", names.join(", "))
        })
}

const QUERIES: &[(&str, Query)] = &[
    ("current", Query::Current),
    ("marked", Query::Marked),
    ("queue", Query::Queue),
    ("state", Query::State),
];

const TOPICS: &[(&str, Topic)] = &[
    ("image", Topic::Image),
    ("marks", Topic::Marks),
    ("mode", Topic::Mode),
];

/// A string or an array of strings.
fn strings(v: &Value, what: &str) -> Result<Vec<String>, String> {
    match v {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Array(a) => a
            .iter()
            .map(|e| {
                e.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("`{what}` must be a string or an array of strings"))
            })
            .collect(),
        _ => Err(format!("`{what}` must be a string or an array of strings")),
    }
}

pub fn parse_request(line: &str) -> Result<Request, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("invalid JSON: {e}"))?;
    let Value::Object(obj) = v else {
        return Err("expected a JSON object".into());
    };
    let mut keys = obj.keys();
    let (Some(key), None) = (keys.next(), keys.next()) else {
        return Err("expected exactly one of `action`, `query`, `subscribe`".into());
    };
    let val = &obj[key];
    match key.as_str() {
        "action" => {
            let actions = strings(val, "action")?
                .iter()
                .map(|s| s.parse::<Action>().map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            if actions.is_empty() {
                return Err("no actions".into());
            }
            Ok(Request::Actions(actions))
        }
        "query" => {
            let q = val.as_str().ok_or("`query` must be a string")?;
            Ok(Request::Query(word("query", q, QUERIES)?))
        }
        "subscribe" => {
            let mut topics = Vec::new();
            for t in strings(val, "subscribe")? {
                let t = word("event", &t, TOPICS)?;
                if !topics.contains(&t) {
                    topics.push(t);
                }
            }
            if topics.is_empty() {
                return Err("no events to subscribe to".into());
            }
            Ok(Request::Subscribe(topics))
        }
        other => Err(format!(
            "unknown request `{other}`: expected `action`, `query` or `subscribe`"
        )),
    }
}

// Envelopes are written by hand so `ok` and `event` come first (serde_json sorts keys).

pub fn ok(data: Value) -> String {
    if data.is_null() {
        r#"{"ok":true}"#.into()
    } else {
        format!(r#"{{"ok":true,"data":{data}}}"#)
    }
}

pub fn error(msg: &str) -> String {
    format!(r#"{{"ok":false,"error":{}}}"#, Value::from(msg))
}

pub fn event(topic: Topic, data: Value) -> String {
    format!(r#"{{"event":"{}","data":{data}}}"#, topic.name())
}

pub fn path_json(p: &Path) -> Value {
    Value::String(p.to_string_lossy().into_owned())
}

/// `$XDG_RUNTIME_DIR/remage`, or a per-user directory in `/tmp`.
pub fn socket_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join("remage"),
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            std::env::temp_dir().join(format!("remage-{user}"))
        }
    }
}

fn pid_of(path: &Path) -> Option<u32> {
    path.file_name()?
        .to_str()?
        .strip_suffix(".sock")?
        .parse()
        .ok()
}

fn alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

/// Live instances' sockets, newest first.
fn instances(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| pid_of(p).is_some_and(alive))
        .map(|p| {
            let t = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (t, p)
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.into_iter().map(|(_, p)| p).collect()
}

/// A request from a connection, with where its reply goes.
pub struct Incoming {
    pub request: Request,
    pub reply: Sender<String>,
    /// For subscriptions: where events go after the reply.
    pub events: Option<Sender<String>>,
}

/// The listening socket. Dropping it removes the socket file once replies have gone out.
pub struct Server {
    path: PathBuf,
    /// Replies handed to connection threads but not yet written.
    in_flight: Arc<AtomicUsize>,
}

impl Server {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // A `quit` sent over the socket still gets its reply.
        let until = Instant::now() + Duration::from_millis(200);
        while self.in_flight.load(Ordering::Acquire) > 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Listen on `<socket_dir>/<pid>.sock`; `deliver` hands each request to the event loop.
pub fn serve(deliver: impl Fn(Incoming) -> bool + Send + Sync + 'static) -> anyhow::Result<Server> {
    let dir = socket_dir();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    // Sockets of instances that died without cleaning up.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for p in entries.flatten().map(|e| e.path()) {
            if pid_of(&p).is_some_and(|pid| !alive(pid)) {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    let path = dir.join(format!("{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let in_flight = Arc::new(AtomicUsize::new(0));
    let deliver = Arc::new(deliver);
    let flight = in_flight.clone();
    std::thread::Builder::new()
        .name("remage-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let deliver = deliver.clone();
                let flight = flight.clone();
                let _ = std::thread::Builder::new()
                    .name("remage-ipc-conn".into())
                    .spawn(move || connection(stream, &*deliver, &flight));
            }
        })?;
    Ok(Server { path, in_flight })
}

fn connection(stream: UnixStream, deliver: &dyn Fn(Incoming) -> bool, in_flight: &AtomicUsize) {
    let Ok(read) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read.take(u64::MAX));
    let mut out = stream;
    let mut line = String::new();
    loop {
        line.clear();
        reader.get_mut().set_limit(MAX_REQUEST);
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if !line.ends_with('\n') && line.len() as u64 >= MAX_REQUEST => {
                let _ = writeln!(out, "{}", error("request too long"));
                return;
            }
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let request = match parse_request(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                if writeln!(out, "{}", error(&e)).is_err() {
                    return;
                }
                continue;
            }
        };
        let subscribe = matches!(request, Request::Subscribe(_));
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let (events_tx, events_rx) = if subscribe {
            let (tx, rx) = crossbeam_channel::unbounded();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        in_flight.fetch_add(1, Ordering::AcqRel);
        let sent = deliver(Incoming {
            request,
            reply: reply_tx,
            events: events_tx,
        });
        let reply = if sent {
            reply_rx
                .recv_timeout(REPLY_TIMEOUT)
                .unwrap_or_else(|_| error("no reply from the viewer"))
        } else {
            error("the viewer is shutting down")
        };
        let written = writeln!(out, "{reply}").is_ok();
        in_flight.fetch_sub(1, Ordering::AcqRel);
        if !written {
            return;
        }
        if let Some(rx) = events_rx {
            stream_events(&mut out, &rx);
            return;
        }
    }
}

fn stream_events(out: &mut UnixStream, rx: &Receiver<String>) {
    while let Ok(ev) = rx.recv() {
        if writeln!(out, "{ev}").is_err() {
            return;
        }
    }
}

/// `remage msg` arguments.
pub struct ClientArgs {
    pub pid: Option<u32>,
    pub query: Option<String>,
    pub subscribe: Vec<String>,
    pub action: Vec<String>,
}

fn find_socket(pid: Option<u32>) -> Result<PathBuf, String> {
    if let Some(pid) = pid {
        let p = socket_dir().join(format!("{pid}.sock"));
        return if p.exists() {
            Ok(p)
        } else {
            Err(format!(
                "no remage with pid {pid} ({} missing)",
                p.display()
            ))
        };
    }
    if let Some(s) = std::env::var_os("REMAGE_SOCKET").filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(s));
    }
    instances(&socket_dir())
        .into_iter()
        .next()
        .ok_or_else(|| "no running remage found".into())
}

/// Run the client; returns the process exit code.
pub fn client(args: ClientArgs) -> i32 {
    match client_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("remage msg: {e}");
            1
        }
    }
}

fn client_inner(args: ClientArgs) -> Result<i32, String> {
    let request = if let Some(q) = &args.query {
        json!({ "query": q })
    } else if !args.subscribe.is_empty() {
        let topics: Vec<&str> = args
            .subscribe
            .iter()
            .flat_map(|s| s.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        json!({ "subscribe": topics })
    } else if !args.action.is_empty() {
        json!({ "action": args.action.join(" ") })
    } else {
        return Err("nothing to send: give an action, --query or --subscribe".into());
    };
    let path = find_socket(args.pid)?;
    let mut stream = UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(stream, "{request}").map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
        return Err("the viewer closed the connection".into());
    }
    let reply: Value = serde_json::from_str(&line).map_err(|e| format!("bad reply: {e}"))?;
    if reply["ok"] != Value::Bool(true) {
        let msg = reply["error"].as_str().unwrap_or("request failed");
        return Err(msg.to_string());
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if !args.subscribe.is_empty() {
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return Ok(0),
                Ok(_) => {
                    if out.write_all(line.as_bytes()).is_err() || out.flush().is_err() {
                        return Ok(0);
                    }
                }
            }
        }
    }
    print_data(&mut out, &reply["data"]).map_err(|e| e.to_string())?;
    Ok(0)
}

/// Strings and lists of strings print one per line, for shell pipelines; the rest as JSON.
fn print_data(out: &mut impl Write, data: &Value) -> std::io::Result<()> {
    match data {
        Value::Null => Ok(()),
        Value::String(s) => writeln!(out, "{s}"),
        Value::Array(a) if a.iter().all(Value::is_string) => {
            for s in a.iter().filter_map(Value::as_str) {
                writeln!(out, "{s}")?;
            }
            Ok(())
        }
        other => writeln!(out, "{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_parse() {
        assert_eq!(
            parse_request(r#"{"action":"goto 5"}"#),
            Ok(Request::Actions(vec![Action::Goto(5)]))
        );
        assert_eq!(
            parse_request(r#"{"action":["next","mark on"]}"#),
            Ok(Request::Actions(vec![
                Action::Next(1),
                Action::Mark(crate::action::Switch::On)
            ]))
        );
        assert_eq!(
            parse_request(r#"{"query":"marked"}"#),
            Ok(Request::Query(Query::Marked))
        );
        assert_eq!(
            parse_request(r#"{"subscribe":["image","mode","image"]}"#),
            Ok(Request::Subscribe(vec![Topic::Image, Topic::Mode]))
        );
        assert_eq!(
            parse_request(r#"{"subscribe":"marks"}"#),
            Ok(Request::Subscribe(vec![Topic::Marks]))
        );
        let e = |s: &str| parse_request(s).unwrap_err();
        assert!(e("nope").contains("invalid JSON"));
        assert!(e("[]").contains("object"));
        assert!(e(r#"{"action":"nxt"}"#).contains("did you mean `next`"));
        assert!(e(r#"{"query":"all"}"#).contains("expected current"));
        assert!(e(r#"{"action":"next","query":"state"}"#).contains("exactly one"));
        assert!(e(r#"{"subscribe":[]}"#).contains("no events"));
        assert!(e(r#"{"frob":1}"#).contains("unknown request"));
    }

    #[test]
    fn replies() {
        assert_eq!(ok(Value::Null), r#"{"ok":true}"#);
        assert_eq!(ok(json!(3)), r#"{"ok":true,"data":3}"#);
        assert_eq!(error("x"), r#"{"ok":false,"error":"x"}"#);
        assert_eq!(
            event(Topic::Mode, json!("grid")),
            r#"{"event":"mode","data":"grid"}"#
        );
        let mut out = Vec::new();
        print_data(&mut out, &json!(["a", "b"])).unwrap();
        print_data(&mut out, &json!({"k": 1})).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a\nb\n{\"k\":1}\n");
    }

    #[test]
    fn socket_names() {
        assert_eq!(pid_of(Path::new("/run/remage/123.sock")), Some(123));
        assert_eq!(pid_of(Path::new("/run/remage/x.sock")), None);
        assert!(alive(std::process::id()));
    }

    #[test]
    fn round_trip_over_a_socket() {
        let dir = std::env::temp_dir().join(format!("remage-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let flight = Arc::new(AtomicUsize::new(0));
        let f = flight.clone();
        std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            connection(
                s,
                &|inc: Incoming| {
                    let reply = match inc.request {
                        Request::Query(Query::Current) => ok(json!("/a.png")),
                        Request::Subscribe(_) => {
                            let ev = inc.events.unwrap();
                            let _ = ev.send(event(Topic::Mode, json!("grid")));
                            ok(Value::Null)
                        }
                        _ => error("unexpected"),
                    };
                    inc.reply.send(reply).is_ok()
                },
                &f,
            );
        });
        let mut s = UnixStream::connect(&path).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        writeln!(s, "{{\"query\":\"current\"}}\n\nbad").unwrap();
        r.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), r#"{"ok":true,"data":"/a.png"}"#);
        line.clear();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("invalid JSON"), "{line}");
        writeln!(s, "{{\"subscribe\":\"mode\"}}").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), r#"{"ok":true}"#);
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), r#"{"event":"mode","data":"grid"}"#);
        assert_eq!(flight.load(Ordering::Acquire), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

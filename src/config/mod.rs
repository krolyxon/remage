//! Configuration: schema, compiled-in defaults, includes, validation and live reload.
//!
//! The compiled-in default is `default.toml` itself (`--dump-config` prints it), so every option
//! has a default by construction. Layers merge in order — default, includes, the user file —
//! with tables merged key by key. Each file is first deserialised on its own, which is what
//! gives errors their `file:line:col`.

pub mod theme;
pub mod watch;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

use crate::action::{Action, CATALOG};
use crate::input::keymap::{Bindings, KeyTables};
use crate::input::keys::KeySeq;
use crate::state::AlphaBg;
use crate::state::queue::SortKey;
use crate::state::view::FitMode;
use theme::{Theme, ThemeConfig};

pub const DEFAULT: &str = include_str!("default.toml");

/// Includes nest at most this deep.
const MAX_INCLUDE_DEPTH: usize = 5;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub include: Vec<String>,
    pub general: General,
    pub window: WindowConfig,
    pub image: ImageConfig,
    pub grid: GridConfig,
    pub ui: UiConfig,
    pub animation: AnimationConfig,
    pub theme: ThemeConfig,
    pub keys: Keys,
    pub commands: BTreeMap<String, Command>,
    pub decoders: Vec<Decoder>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StartMode {
    #[default]
    Image,
    Grid,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub open_siblings: bool,
    pub recursive: bool,
    pub sort: SortKey,
    pub reverse: bool,
    pub start_mode: StartMode,
    pub wrap: bool,
    pub watch_dir: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowConfig {
    pub decorations: bool,
    pub title: String,
    pub class: String,
    pub fullscreen: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterMode {
    #[default]
    Auto,
    Linear,
    Nearest,
}

impl FilterMode {
    /// `None` = automatic, otherwise whether to force nearest.
    pub fn nearest(self) -> Option<bool> {
        match self {
            FilterMode::Auto => None,
            FilterMode::Linear => Some(false),
            FilterMode::Nearest => Some(true),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageConfig {
    pub fit: FitMode,
    pub zoom_step: f32,
    pub filter: FilterMode,
    pub alpha_background: AlphaBg,
    pub auto_orient: bool,
    pub color_management: bool,
    pub preload: usize,
    pub cache_mb: usize,
    pub slideshow_delay: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GridConfig {
    pub sizes: Vec<u32>,
    pub size: u32,
    pub gap: u32,
    pub thumbnail_cache: bool,
    pub names: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub statusbar: bool,
    pub which_key_delay_ms: u64,
    pub scale: f32,
    pub statusbar_left: String,
    pub statusbar_right: String,
    pub info_command: String,
    pub font_ui: String,
    pub font_mono: String,
    pub font_display: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Transition {
    #[default]
    Fade,
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnimationConfig {
    pub enabled: bool,
    pub duration_ms: u64,
    pub transition: Transition,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    pub global: BindingTable,
    pub image: BindingTable,
    pub grid: BindingTable,
    pub visual: BindingTable,
}

impl Keys {
    fn tables(&self) -> KeyTables {
        KeyTables {
            global: self.global.bindings.clone(),
            image: self.image.bindings.clone(),
            grid: self.grid.bindings.clone(),
            visual: self.visual.bindings.clone(),
        }
    }

    fn first_error(&self) -> Option<&(std::ops::Range<usize>, String)> {
        [&self.global, &self.image, &self.grid, &self.visual]
            .into_iter()
            .flat_map(|t| t.errors.first())
            .min_by_key(|(span, _)| span.start)
    }
}

/// A `[keys.<mode>]` table in file order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BindingTable {
    pub bindings: Bindings,
    /// Bad key notation and duplicates, with the key's byte span.
    pub errors: Vec<(std::ops::Range<usize>, String)>,
}

impl<'de> Deserialize<'de> for BindingTable {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = BindingTable;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a table of key = action")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<BindingTable, A::Error> {
                let mut out = BindingTable::default();
                while let Some(k) = map.next_key::<toml::Spanned<String>>()? {
                    let actions = map.next_value::<Binding>()?.0;
                    // serde errors can't point at a key, so key errors are kept with their
                    // span and reported by `Layer::parse`.
                    let seq: KeySeq = match k.get_ref().parse() {
                        Ok(seq) => seq,
                        Err(e) => {
                            out.errors.push((k.span(), e));
                            continue;
                        }
                    };
                    if let Some((prev, _)) = out.bindings.iter().find(|(s, _)| *s == seq) {
                        let msg = format!(
                            "`{}` is bound twice in this table (also as `{prev}`)",
                            k.get_ref()
                        );
                        out.errors.push((k.span(), msg));
                        continue;
                    }
                    out.bindings.push((seq, actions));
                }
                Ok(out)
            }
        }
        d.deserialize_map(V)
    }
}

/// One binding value: an action, an array of actions, or `"none"`.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding(pub Option<Vec<Action>>);

impl<'de> Deserialize<'de> for Binding {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let list = ActionList::deserialize(d)?;
        Ok(Binding(list.0))
    }
}

/// An action or an array of them; `"none"` (or `[]`) is `None`.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionList(pub Option<Vec<Action>>);

impl<'de> Deserialize<'de> for ActionList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        fn parse<E: de::Error>(s: &str) -> Result<Action, E> {
            s.parse().map_err(|e| E::custom(e))
        }
        impl<'de> Visitor<'de> for V {
            type Value = ActionList;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an action, an array of actions, or \"none\"")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<ActionList, E> {
                if s.trim() == "none" {
                    return Ok(ActionList(None));
                }
                Ok(ActionList(Some(vec![parse(s)?])))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ActionList, A::Error> {
                let mut out = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    out.push(parse(&s)?);
                }
                Ok(ActionList((!out.is_empty()).then_some(out)))
            }
        }
        d.deserialize_any(V)
    }
}

impl Default for ActionList {
    fn default() -> Self {
        ActionList(Some(vec![Action::Refresh]))
    }
}

/// A command line: a string runs through `sh -c`, an array is exec'd directly.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum CommandLine {
    Shell(String),
    Exec(Vec<String>),
}

impl Default for CommandLine {
    fn default() -> Self {
        CommandLine::Exec(Vec::new())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Targets {
    #[default]
    Auto,
    Current,
    Marked,
    All,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stdin {
    #[default]
    None,
    Files,
    Files0,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    #[default]
    Status,
    Actions,
    Ignore,
}

/// A `[commands.<name>]` entry. Execution arrives with M4.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Command {
    pub desc: String,
    pub run: CommandLine,
    pub targets: Targets,
    pub stdin: Stdin,
    pub confirm: Option<String>,
    pub output: Output,
    pub after: ActionList,
}

/// A `[[decoders]]` entry. Used from M5.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Decoder {
    #[serde(rename = "match")]
    pub patterns: Vec<String>,
    pub run: Vec<String>,
}

/// A successfully loaded configuration.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub config: Config,
    /// Key tables per layer, lowest priority first.
    pub keys: Vec<KeyTables>,
    pub theme: Theme,
    pub warnings: Vec<String>,
    /// Files whose changes should trigger a reload.
    pub watch: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct LoadError {
    pub message: String,
    pub watch: Vec<PathBuf>,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// `$XDG_CONFIG_HOME/remage/config.toml`, falling back to `~/.config/remage/`.
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("remage").join("config.toml"))
}

/// The closest candidate within a small edit distance, for "did you mean" hints.
pub fn suggest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let word = word.to_lowercase();
    candidates
        .into_iter()
        .map(|c| (levenshtein(&word, &c.to_lowercase()), c))
        .filter(|(d, _)| *d > 0 && (*d <= 2 || *d * 3 <= word.chars().count()))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// Expand `~` and `$VAR` / `${VAR}`; relative paths resolve against `base`.
pub fn expand_path(s: &str, base: &Path) -> PathBuf {
    let mut out = String::new();
    let mut rest = s;
    if let Some(r) = rest.strip_prefix('~')
        && (r.is_empty() || r.starts_with('/'))
        && let Some(home) = dirs::home_dir()
    {
        out.push_str(&home.to_string_lossy());
        rest = r;
    }
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, used) = if let Some(b) = after.strip_prefix('{') {
            match b.find('}') {
                Some(end) => (&b[..end], end + 2),
                None => ("", 0),
            }
        } else {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..end], end)
        };
        if name.is_empty() {
            out.push('$');
            rest = after;
        } else {
            out.push_str(&std::env::var(name).unwrap_or_default());
            rest = &after[used..];
        }
    }
    out.push_str(rest);
    let p = PathBuf::from(out);
    if p.is_relative() { base.join(p) } else { p }
}

fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(src.len());
    let before = &src[..offset];
    let line = before.matches('\n').count() + 1;
    let col = before
        .rfind('\n')
        .map_or(before, |i| &before[i + 1..])
        .chars()
        .count()
        + 1;
    (line, col)
}

/// Add a "did you mean" to serde's unknown field / variant messages.
fn with_hint(msg: &str) -> String {
    let Some(rest) = msg
        .strip_prefix("unknown field `")
        .or_else(|| msg.strip_prefix("unknown variant `"))
    else {
        return msg.to_string();
    };
    let Some((word, expected)) = rest.split_once('`') else {
        return msg.to_string();
    };
    let options: Vec<&str> = expected.split('`').skip(1).step_by(2).collect();
    match suggest(word, options) {
        Some(s) => format!("{msg}; did you mean `{s}`?"),
        None => msg.to_string(),
    }
}

fn located(file: &Path, src: &str, span: Option<std::ops::Range<usize>>, msg: &str) -> String {
    let msg = with_hint(msg.trim_end());
    match span {
        Some(s) => {
            let (l, c) = line_col(src, s.start);
            format!("{}:{l}:{c}: {msg}", file.display())
        }
        None => format!("{}: {msg}", file.display()),
    }
}

/// One parsed config file.
struct Layer {
    file: PathBuf,
    src: String,
    table: toml::Table,
    keys: KeyTables,
}

impl Layer {
    fn parse(file: PathBuf, src: String) -> Result<(Layer, Config), String> {
        let typed: Config =
            toml::from_str(&src).map_err(|e| located(&file, &src, e.span(), e.message()))?;
        if let Some((span, msg)) = typed.keys.first_error() {
            return Err(located(&file, &src, Some(span.clone()), msg));
        }
        let table: toml::Table =
            toml::from_str(&src).map_err(|e| located(&file, &src, e.span(), e.message()))?;
        let keys = typed.keys.tables();
        Ok((
            Layer {
                file,
                src,
                table,
                keys,
            },
            typed,
        ))
    }

    /// `file:line:col` of a dotted path in this file, if it sets it.
    fn locate(&self, path: &[&str], msg: &str) -> Option<String> {
        let span = span_of(&self.src, path, |_| true)?;
        Some(located(&self.file, &self.src, Some(span), msg))
    }
}

type DeTable<'i> = toml::de::DeTable<'i>;
type DeValue<'i> = toml::de::DeValue<'i>;

fn entry<'a, 'i>(t: &'a DeTable<'i>, key: &str) -> Option<&'a toml::Spanned<DeValue<'i>>> {
    t.iter().find(|(k, _)| k.get_ref() == key).map(|(_, v)| v)
}

/// The span of the value at a dotted path; with `pick`, of the first entry in the table at
/// `path` whose value it accepts.
fn span_of(
    src: &str,
    path: &[&str],
    pick: impl Fn(&DeValue) -> bool,
) -> Option<std::ops::Range<usize>> {
    let doc = DeTable::parse(src).ok()?;
    let (last, init) = path.split_last()?;
    let mut table = doc.get_ref();
    for p in init {
        table = entry(table, p)?.get_ref().as_table()?;
    }
    let v = entry(table, last)?;
    match v.get_ref().as_table() {
        Some(t) if path.len() > 1 && path[0] == "keys" => t
            .iter()
            .find(|(_, v)| pick(v.get_ref()))
            .map(|(_, v)| v.span()),
        _ => Some(v.span()),
    }
}

struct Loader {
    layers: Vec<Layer>,
    warnings: Vec<String>,
    watch: Vec<PathBuf>,
    stack: Vec<PathBuf>,
}

impl Loader {
    fn watch(&mut self, path: &Path) {
        for p in [Some(path.to_path_buf()), std::fs::canonicalize(path).ok()]
            .into_iter()
            .flatten()
        {
            if !self.watch.contains(&p) {
                self.watch.push(p);
            }
        }
    }

    fn file(&mut self, path: &Path) -> Result<(), String> {
        self.watch(path);
        let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (layer, typed) = Layer::parse(path.to_path_buf(), src)?;
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.stack.push(canon);
        let base = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        for inc in &typed.include {
            let p = expand_path(inc, &base);
            let canon = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            if self.stack.contains(&canon) {
                let msg = format!("include cycle through {}", p.display());
                return Err(layer.locate(&["include"], &msg).unwrap_or(msg));
            }
            if self.stack.len() > MAX_INCLUDE_DEPTH {
                let msg = format!("includes nest deeper than {MAX_INCLUDE_DEPTH}");
                return Err(layer.locate(&["include"], &msg).unwrap_or(msg));
            }
            if p.is_file() {
                self.file(&p)?;
            } else {
                // Watched anyway: matugen or pywal may create it later.
                self.watch(&p);
                self.warnings.push(format!(
                    "{}: include not found: {}",
                    path.display(),
                    p.display()
                ));
            }
        }
        self.stack.pop();
        self.layers.push(layer);
        Ok(())
    }

    /// The last layer that sets `path`, formatted as an error there.
    fn error_at(&self, path: &[&str], msg: &str) -> String {
        self.layers
            .iter()
            .rev()
            .find_map(|l| l.locate(path, msg))
            .unwrap_or_else(|| format!("{}: {msg}", path.join(".")))
    }
}

/// Merge `over` into `base`, table by table. Commands replace whole, not field by field.
fn merge(base: &mut toml::Table, over: toml::Table, depth: usize, key: &str) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o))
                if !(depth == 1 && key == "commands") =>
            {
                merge(b, o, depth + 1, &k);
            }
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

fn default_layer() -> (Layer, Config) {
    match Layer::parse(PathBuf::from("<default>"), DEFAULT.to_string()) {
        Ok(l) => l,
        // The default is tested; this only guards against a broken build.
        Err(e) => panic!("built-in default config: {e}"),
    }
}

/// The compiled-in defaults alone.
pub fn defaults() -> Loaded {
    let (layer, config) = default_layer();
    Loaded {
        theme: theme::resolve(&config.theme, None),
        keys: vec![layer.keys],
        config,
        warnings: Vec::new(),
        watch: Vec::new(),
    }
}

/// Load the config at `path` (or the default location) over the defaults. An explicit path
/// must exist; a missing default file just means defaults.
pub fn load(path: Option<&Path>) -> Result<Loaded, LoadError> {
    let explicit = path.is_some();
    let path = path.map(Path::to_path_buf).or_else(default_path);
    let mut loader = Loader {
        layers: vec![default_layer().0],
        warnings: Vec::new(),
        watch: Vec::new(),
        stack: Vec::new(),
    };
    let result = (|| {
        if let Some(path) = &path {
            if path.is_file() || explicit {
                loader.file(path)?;
            } else {
                loader.watch(path);
            }
        }
        finish(&mut loader)
    })();
    result.map_err(|message| LoadError {
        message,
        watch: loader.watch.clone(),
    })
}

fn finish(loader: &mut Loader) -> Result<Loaded, String> {
    let mut table = toml::Table::new();
    for layer in &loader.layers {
        let mut t = layer.table.clone();
        t.remove("keys");
        t.remove("include");
        merge(&mut table, t, 0, "");
    }
    let config: Config = toml::Value::Table(table)
        .try_into()
        .map_err(|e: toml::de::Error| format!("merged config: {}", e.message()))?;
    validate(&config, loader)?;

    let pywal = match &config.theme.pywal {
        Some(p) => {
            let base = loader.layers.last().and_then(|l| l.file.parent());
            let path = expand_path(p, base.unwrap_or(Path::new("/")));
            loader.watch(&path);
            match theme::read_pywal(&path) {
                Ok(colors) => Some(colors),
                Err(e) => {
                    loader.warnings.push(format!("pywal: {e}"));
                    None
                }
            }
        }
        None => None,
    };
    Ok(Loaded {
        theme: theme::resolve(&config.theme, pywal),
        keys: loader.layers.iter().map(|l| l.keys.clone()).collect(),
        config,
        warnings: loader.warnings.clone(),
        watch: loader.watch.clone(),
    })
}

/// Range checks and cross-references that the types alone can't express.
fn validate(c: &Config, loader: &Loader) -> Result<(), String> {
    let check = |ok: bool, path: &[&str], msg: &str| -> Result<(), String> {
        if ok {
            Ok(())
        } else {
            Err(loader.error_at(path, msg))
        }
    };
    let i = &c.image;
    check(
        i.zoom_step > 1.0 && i.zoom_step <= 10.0,
        &["image", "zoom_step"],
        "zoom_step must be above 1.0 and at most 10",
    )?;
    check(
        i.preload <= 16,
        &["image", "preload"],
        "preload must be at most 16",
    )?;
    check(
        i.cache_mb >= 16,
        &["image", "cache_mb"],
        "cache_mb must be at least 16",
    )?;
    check(
        i.slideshow_delay.is_finite() && i.slideshow_delay > 0.0,
        &["image", "slideshow_delay"],
        "slideshow_delay must be positive",
    )?;
    let g = &c.grid;
    check(
        !g.sizes.is_empty()
            && g.sizes.windows(2).all(|w| w[0] < w[1])
            && g.sizes.iter().all(|s| (16..=1024).contains(s)),
        &["grid", "sizes"],
        "sizes must be ascending, each between 16 and 1024",
    )?;
    check(
        (16..=1024).contains(&g.size),
        &["grid", "size"],
        "size must be between 16 and 1024",
    )?;
    check(g.gap <= 256, &["grid", "gap"], "gap must be at most 256")?;
    check(
        c.ui.scale.is_finite() && (0.25..=4.0).contains(&c.ui.scale),
        &["ui", "scale"],
        "scale must be between 0.25 and 4",
    )?;
    check(
        c.animation.duration_ms <= 2000,
        &["animation", "duration_ms"],
        "duration_ms must be at most 2000",
    )?;
    check(
        !c.window.class.is_empty(),
        &["window", "class"],
        "class must not be empty",
    )?;
    for (name, cmd) in &c.commands {
        let empty = match &cmd.run {
            CommandLine::Shell(s) => s.trim().is_empty(),
            CommandLine::Exec(v) => v.is_empty(),
        };
        check(
            !empty,
            &["commands", name],
            &format!("command `{name}` needs a `run`"),
        )?;
    }
    // `run <name>` must name a command.
    for layer in &loader.layers {
        for (mode, table) in [
            ("global", &layer.keys.global),
            ("image", &layer.keys.image),
            ("grid", &layer.keys.grid),
            ("visual", &layer.keys.visual),
        ] {
            for (_, actions) in table {
                for a in actions.iter().flatten() {
                    if let Action::Run(name) = a
                        && !c.commands.contains_key(name)
                    {
                        let hint = suggest(name, c.commands.keys().map(String::as_str))
                            .map(|s| format!(", did you mean `{s}`?"))
                            .unwrap_or_default();
                        let msg = format!("no command named `{name}`{hint}");
                        let loc = span_of(&layer.src, &["keys", mode], |v| mentions(v, name))
                            .map(|span| located(&layer.file, &layer.src, Some(span), &msg));
                        return Err(loc.unwrap_or(msg));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Whether a binding value contains `run <name>`.
fn mentions(v: &DeValue, name: &str) -> bool {
    let is_run = |s: &str| matches!(s.parse(), Ok(Action::Run(n)) if n == name);
    match v {
        DeValue::String(s) => is_run(s),
        DeValue::Array(a) => a.iter().any(|e| e.get_ref().as_str().is_some_and(is_run)),
        _ => false,
    }
}

/// The full commented default, plus a reference of every action.
pub fn dump() -> String {
    let mut out = DEFAULT.to_string();
    out.push_str("\n# Actions, for key bindings, the : palette, IPC and command output:\n#\n");
    for e in CATALOG {
        let usage = if e.args.is_empty() {
            e.name.to_string()
        } else {
            format!("{} {}", e.name, e.args)
        };
        out.push_str(&format!("#   {usage:<46} {}\n", e.desc));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keymap::Keymap;
    use crate::state::Mode;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("remage-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, src: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, src).unwrap();
        p
    }

    fn err(src: &str) -> String {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = tmp(&format!("err{n}"));
        let p = write(&dir, "config.toml", src);
        load(Some(&p))
            .unwrap_err()
            .message
            .replace(&dir.display().to_string(), "")
    }

    #[test]
    fn default_parses_and_is_complete() {
        let d = defaults();
        assert_eq!(d.config.image.fit, FitMode::Down);
        assert_eq!(d.config.grid.sizes, [96, 128, 160, 224, 320]);
        assert!(d.config.commands.is_empty());
        // Every field of the plain sections is spelled out in default.toml, so none of the
        // placeholder serde defaults can leak through.
        let doc: toml::Table = toml::from_str(DEFAULT).unwrap();
        let sections: [(&str, toml::Value); 6] = [
            ("general", toml::Value::try_from(&d.config.general).unwrap()),
            ("window", toml::Value::try_from(&d.config.window).unwrap()),
            ("image", toml::Value::try_from(&d.config.image).unwrap()),
            ("grid", toml::Value::try_from(&d.config.grid).unwrap()),
            ("ui", toml::Value::try_from(&d.config.ui).unwrap()),
            (
                "animation",
                toml::Value::try_from(&d.config.animation).unwrap(),
            ),
        ];
        for (name, v) in sections {
            let written = doc[name].as_table().unwrap();
            for key in v.as_table().unwrap().keys() {
                assert!(written.contains_key(key), "default.toml lacks {name}.{key}");
            }
        }
        assert!(doc["theme"].as_table().unwrap().contains_key("preset"));
    }

    #[test]
    fn default_validates() {
        let mut loader = Loader {
            layers: vec![default_layer().0],
            warnings: Vec::new(),
            watch: Vec::new(),
            stack: Vec::new(),
        };
        finish(&mut loader).unwrap();
    }

    #[test]
    fn partial_file_merges_over_defaults() {
        let dir = tmp("partial");
        let p = write(&dir, "config.toml", "[image]\nfit = \"width\"\n");
        let l = load(Some(&p)).unwrap();
        assert_eq!(l.config.image.fit, FitMode::Width);
        assert_eq!(l.config.image.zoom_step, 1.25);
        assert_eq!(l.config.window.class, "remage");
        assert!(l.watch.contains(&p));
    }

    #[test]
    fn errors_carry_file_line_col_and_hints() {
        assert_eq!(
            err("[image]\nfitt = \"down\"\n"),
            "/config.toml:2:1: unknown field `fitt`, expected one of `fit`, `zoom_step`, \
             `filter`, `alpha_background`, `auto_orient`, `color_management`, `preload`, \
             `cache_mb`, `slideshow_delay`; did you mean `fit`?"
        );
        let e = err("[image]\nfit = \"wdth\"\n");
        assert!(
            e.starts_with("/config.toml:2:7: unknown variant `wdth`"),
            "{e}"
        );
        assert!(e.ends_with("did you mean `width`?"), "{e}");
        let e = err("[keys.global]\nx = \"nxt\"\n");
        assert!(
            e.starts_with("/config.toml:2:5: unknown action `nxt`"),
            "{e}"
        );
        let e = err("[keys.global]\n\"<C-Bogus>\" = \"next\"\n");
        assert!(e.contains("/config.toml:2:"), "{e}");
        assert!(e.contains("unknown key `<C-Bogus>`"), "{e}");
        let e = err("[image]\nzoom_step = 0.5\n");
        assert!(e.starts_with("/config.toml:2:13: zoom_step"), "{e}");
        let e = err("[keys.image]\n\"<Space>w\" = \"run wallpaper\"\n");
        assert!(
            e.starts_with("/config.toml:2:14: no command named `wallpaper`"),
            "{e}"
        );
        let e = err("[theme]\naccent = \"#12345\"\n");
        assert!(e.starts_with("/config.toml:2:10:"), "{e}");
        let e = err("[keys.global]\n\"G\" = \"last\"\n\"<S-g>\" = \"first\"\n");
        assert!(e.contains("bound twice"), "{e}");
        let e = err("[image\n");
        assert!(e.starts_with("/config.toml:1:"), "{e}");
    }

    #[test]
    fn includes_load_in_order_and_the_includer_wins() {
        let dir = tmp("include");
        write(
            &dir,
            "a.toml",
            "[theme]\nbackground = \"#101010\"\naccent = \"#00ff00\"\n[image]\nfit = \"fill\"\n",
        );
        write(&dir, "b.toml", "[theme]\naccent = \"#0000ff\"\n");
        let p = write(
            &dir,
            "config.toml",
            "include = [\"a.toml\", \"b.toml\", \"missing.toml\"]\n[image]\nfit = \"width\"\n\
             [commands.wallpaper]\nrun = [\"true\"]\n\
             [keys.global]\n\"<Space>w\" = \"run wallpaper\"\n",
        );
        let l = load(Some(&p)).unwrap();
        assert_eq!(l.config.image.fit, FitMode::Width);
        assert_eq!(l.theme.background, theme::Rgba::rgb(0x101010));
        assert_eq!(l.theme.accent, theme::Rgba::rgb(0x0000ff));
        assert_eq!(l.warnings.len(), 1);
        assert!(l.warnings[0].contains("missing.toml"));
        assert!(l.watch.contains(&dir.join("missing.toml")));
        assert!(l.watch.contains(&dir.join("a.toml")));
        let refs: Vec<&KeyTables> = l.keys.iter().collect();
        let km = Keymap::new(&refs);
        let w: KeySeq = "<Space>w".parse().unwrap();
        assert_eq!(
            km.lookup(Mode::Image, &w.0),
            Some(&[Action::Run("wallpaper".into())][..])
        );
    }

    #[test]
    fn include_cycles_are_errors() {
        let dir = tmp("cycle");
        write(&dir, "a.toml", "include = [\"config.toml\"]\n");
        let p = write(&dir, "config.toml", "include = [\"a.toml\"]\n");
        assert!(load(Some(&p)).unwrap_err().message.contains("cycle"));
    }

    #[test]
    fn commands_replace_whole() {
        let dir = tmp("commands");
        write(
            &dir,
            "a.toml",
            "[commands.rm]\nrun = [\"rm\", \"{files}\"]\nconfirm = \"Sure?\"\n",
        );
        let p = write(
            &dir,
            "config.toml",
            "include = [\"a.toml\"]\n[commands.rm]\nrun = \"trash {files}\"\n",
        );
        let l = load(Some(&p)).unwrap();
        let rm = &l.config.commands["rm"];
        assert_eq!(rm.confirm, None);
        assert_eq!(rm.run, CommandLine::Shell("trash {files}".into()));
        assert_eq!(rm.after, ActionList(Some(vec![Action::Refresh])));
    }

    #[test]
    fn missing_explicit_file_is_an_error() {
        let dir = tmp("missing");
        assert!(load(Some(&dir.join("nope.toml"))).is_err());
    }

    #[test]
    fn path_expansion() {
        let base = Path::new("/etc/remage");
        // SAFETY: tests in this module don't read REMAGE_TEST_DIR concurrently.
        unsafe { std::env::set_var("REMAGE_TEST_DIR", "/opt/x") };
        assert_eq!(
            expand_path("a.toml", base),
            PathBuf::from("/etc/remage/a.toml")
        );
        assert_eq!(
            expand_path("$REMAGE_TEST_DIR/a", base),
            PathBuf::from("/opt/x/a")
        );
        assert_eq!(
            expand_path("${REMAGE_TEST_DIR}b", base),
            PathBuf::from("/opt/xb")
        );
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_path("~/c", base), home.join("c"));
        assert_eq!(expand_path("/abs", base), PathBuf::from("/abs"));
    }

    #[test]
    fn suggestions() {
        assert_eq!(suggest("nxt", ["next", "prev"]), Some("next"));
        assert_eq!(suggest("zzzzzz", ["next", "prev"]), None);
        assert_eq!(suggest("next", ["next"]), None);
    }

    #[test]
    fn dump_lists_every_action() {
        let d = dump();
        for e in CATALOG {
            assert!(d.contains(&format!("#   {}", e.name)), "{}", e.name);
        }
        // The dump is itself a valid config.
        toml::from_str::<Config>(&d).unwrap();
    }
}

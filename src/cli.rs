//! Command-line interface.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::state::queue::SortKey;
use crate::state::view::FitMode;

#[derive(Parser, Debug)]
#[command(
    name = "remage",
    version,
    about = "Keyboard-centric image viewer",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Sub>,

    /// Files or directories to open [default: .]
    pub paths: Vec<PathBuf>,

    /// Read file paths from stdin, one per line
    #[arg(short = 'i', long)]
    pub stdin: bool,

    /// Paths on stdin and printed by -o are NUL-separated
    #[arg(short = '0', long)]
    pub null: bool,

    /// Print the marked files on exit
    #[arg(short = 'o', long)]
    pub print_marked: bool,

    /// Descend into directories
    #[arg(short, long)]
    pub recursive: bool,

    /// Start in the thumbnail grid
    #[arg(short = 't', long)]
    pub grid: bool,

    /// Read the shared thumbnail cache but never write to it
    #[arg(short = 'p', long)]
    pub private: bool,

    /// Start at the Nth image (1-based)
    #[arg(short = 'n', long, value_name = "N")]
    pub start: Option<usize>,

    /// Start in fullscreen
    #[arg(short, long)]
    pub fullscreen: bool,

    /// Hide the status bar
    #[arg(short = 'b', long)]
    pub no_bar: bool,

    /// Start a slideshow, optionally with this delay: -S=3 (the = keeps paths apart)
    #[arg(
        short = 'S',
        long,
        value_name = "SECS",
        num_args = 0..=1,
        require_equals = true
    )]
    pub slideshow: Option<Option<f32>>,

    /// How images fit the window
    #[arg(short = 's', long, value_enum, value_name = "MODE")]
    pub fit: Option<FitMode>,

    /// Fixed zoom level in percent (overrides --fit)
    #[arg(short = 'z', long, value_name = "PCT")]
    pub zoom: Option<f32>,

    /// Initial window size in logical pixels
    #[arg(short = 'g', long, value_name = "WxH", value_parser = parse_geometry)]
    pub geometry: Option<(u32, u32)>,

    /// Wayland app_id / X11 class [default: remage]
    #[arg(long, value_name = "NAME")]
    pub class: Option<String>,

    /// Sort order for directory contents [default: natural]
    #[arg(long, value_enum, value_name = "KEY")]
    pub sort: Option<SortKey>,

    /// Reverse the sort order
    #[arg(long)]
    pub reverse: bool,

    /// Open only the named file, not the rest of its directory
    #[arg(long)]
    pub no_siblings: bool,

    /// Config file [default: $XDG_CONFIG_HOME/remage/config.toml]
    #[arg(short = 'c', long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Print the full commented default config and exit
    #[arg(long)]
    pub dump_config: bool,

    /// Validate the config and exit non-zero on errors
    #[arg(long)]
    pub check_config: bool,
}

#[derive(Subcommand, Debug)]
pub enum Sub {
    /// Send an action or query to a running remage
    Msg {
        /// The instance to talk to [default: $REMAGE_SOCKET, else the newest]
        #[arg(long, value_name = "PID")]
        pid: Option<u32>,

        /// Print current, marked, queue or state
        #[arg(long, value_name = "Q", conflicts_with_all = ["subscribe", "action"])]
        query: Option<String>,

        /// Stream events (image, marks, mode) as JSON lines
        #[arg(long, value_name = "EV", conflicts_with = "action")]
        subscribe: Vec<String>,

        /// The action, e.g. `goto 5`
        #[arg(
            value_name = "ACTION",
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        action: Vec<String>,
    },
}

fn parse_geometry(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or("expected WxH, e.g. 1280x800")?;
    let parse = |v: &str| v.trim().parse::<u32>().ok().filter(|n| *n > 0);
    match (parse(w), parse(h)) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err("expected positive WxH, e.g. 1280x800".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn msg_subcommand() {
        let c = Cli::try_parse_from(["remage", "msg", "goto", "5"]).unwrap();
        match c.command {
            Some(Sub::Msg { action, .. }) => assert_eq!(action, ["goto", "5"]),
            other => panic!("{other:?}"),
        }
        let c = Cli::try_parse_from(["remage", "msg", "slideshow-delay", "-1"]).unwrap();
        assert!(matches!(c.command, Some(Sub::Msg { action, .. }) if action[1] == "-1"));
        assert!(Cli::try_parse_from(["remage", "msg", "--query", "x", "next"]).is_err());
        let c = Cli::try_parse_from(["remage", "-io0", "a.png"]).unwrap();
        assert!(c.stdin && c.print_marked && c.null && c.command.is_none());
    }

    #[test]
    fn slideshow_seconds_need_an_equals() {
        let c = Cli::try_parse_from(["remage", "-S", "pics"]).unwrap();
        assert_eq!(c.slideshow, Some(None));
        assert_eq!(c.paths, [PathBuf::from("pics")]);
        let c = Cli::try_parse_from(["remage", "-S=2.5", "pics"]).unwrap();
        assert_eq!(c.slideshow, Some(Some(2.5)));
        let c = Cli::try_parse_from(["remage", "--slideshow=3"]).unwrap();
        assert_eq!(c.slideshow, Some(Some(3.0)));
        assert_eq!(Cli::try_parse_from(["remage"]).unwrap().slideshow, None);
    }

    #[test]
    fn geometry() {
        assert_eq!(parse_geometry("800x600"), Ok((800, 600)));
        assert!(parse_geometry("800").is_err());
        assert!(parse_geometry("0x600").is_err());
    }
}

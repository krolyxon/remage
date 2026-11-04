//! Command-line interface.

use std::path::PathBuf;

use clap::Parser;

use crate::state::queue::SortKey;
use crate::state::view::FitMode;

#[derive(Parser, Debug)]
#[command(name = "remage", version, about = "Keyboard-centric image viewer")]
pub struct Cli {
    /// Files or directories to open [default: .]
    pub paths: Vec<PathBuf>,

    /// Descend into directories
    #[arg(short, long)]
    pub recursive: bool,

    /// Start at the Nth image (1-based)
    #[arg(short = 'n', long, value_name = "N")]
    pub start: Option<usize>,

    /// Start in fullscreen
    #[arg(short, long)]
    pub fullscreen: bool,

    /// Hide the status bar
    #[arg(short = 'b', long)]
    pub no_bar: bool,

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
    fn geometry() {
        assert_eq!(parse_geometry("800x600"), Ok((800, 600)));
        assert!(parse_geometry("800").is_err());
        assert!(parse_geometry("0x600").is_err());
    }
}

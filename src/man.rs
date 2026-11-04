//! The man page: clap's sections plus keys and actions generated from the compiled-in defaults.

use std::io::Write;

use clap::CommandFactory;
use clap_mangen::Man;
use clap_mangen::roff::{Roff, bold, italic, roman};

use crate::action::CATALOG;
use crate::cli::Cli;
use crate::config;
use crate::input::keymap::Bindings;

pub fn render(w: &mut dyn Write) -> std::io::Result<()> {
    let man = Man::new(Cli::command()).manual("remage manual");
    man.render_title(w)?;
    man.render_name_section(w)?;
    man.render_synopsis_section(w)?;
    man.render_description_section(w)?;
    man.render_options_section(w)?;
    man.render_subcommands_section(w)?;

    let mut r = Roff::new();
    keys(&mut r);
    actions(&mut r);
    prose(&mut r);
    r.to_writer(w)?;

    man.render_version_section(w)?;
    Ok(())
}

fn section(r: &mut Roff, title: &str) {
    r.control("SH", [title]);
}

fn item(r: &mut Roff, term: Vec<clap_mangen::roff::Inline>, body: &str) {
    r.control("TP", []);
    r.text(term);
    r.text([roman(body)]);
}

fn para(r: &mut Roff, text: &str) {
    r.control("PP", []);
    r.text([roman(text)]);
}

/// The default bindings per mode table, keys for the same action merged as in `?` help.
fn keys(r: &mut Roff) {
    section(r, "KEYS");
    para(
        r,
        "The default bindings. Mode tables override global ones, and visual mode also inherits \
         the grid table. Notation follows vim. A count typed first applies to the action \
         (5n, 12G, 3+). Press ? in remage for the live keymap, including your own bindings.",
    );
    let Some(tables) = config::defaults().keys.into_iter().next() else {
        return;
    };
    let modes: [(&str, &Bindings); 4] = [
        ("Global", &tables.global),
        ("Image mode", &tables.image),
        ("Grid mode", &tables.grid),
        ("Visual mode", &tables.visual),
    ];
    for (title, table) in modes {
        r.control("SS", [title]);
        // (catalog rank, action, keys, description)
        let mut rows: Vec<(usize, String, String, String)> = Vec::new();
        for (seq, actions) in table {
            let Some(actions) = actions else { continue };
            let Some(first) = actions.first() else {
                continue;
            };
            let desc = actions
                .iter()
                .map(|a| a.description())
                .collect::<Vec<_>>()
                .join(", then ");
            let k = crate::ui::pretty_keys(&seq.to_string());
            match rows.iter_mut().find(|row| row.3 == desc) {
                Some(row) => {
                    row.2.push_str(", ");
                    row.2.push_str(&k);
                }
                None => {
                    let rank = CATALOG
                        .iter()
                        .position(|e| e.name == first.name())
                        .unwrap_or(usize::MAX);
                    rows.push((rank, first.to_string(), k, desc));
                }
            }
        }
        // Catalog order, then by action (`next` before `next 10`), as in `?` help.
        rows.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        for (_, _, k, desc) in rows {
            item(r, vec![bold(k)], &desc);
        }
    }
}

/// Every action in the grammar shared by keys, the : palette, IPC and command output.
fn actions(r: &mut Roff) {
    section(r, "ACTIONS");
    para(
        r,
        "Keys, the : palette, remage msg and commands with output = \"actions\" all speak the \
         same grammar: a name and its arguments.",
    );
    for e in CATALOG {
        let mut term = vec![bold(e.name)];
        if !e.args.is_empty() {
            term.push(roman(" "));
            term.push(italic(e.args));
        }
        item(r, term, e.desc);
    }
}

fn prose(r: &mut Roff) {
    section(r, "CONFIGURATION");
    para(
        r,
        "remage reads $XDG_CONFIG_HOME/remage/config.toml (or ~/.config/remage/config.toml, or \
         the file given with -c). The file is merged over the compiled-in defaults, so a partial \
         file is fine; remage --dump-config prints the complete commented default and \
         remage --check-config validates a file. Changes apply live.",
    );
    para(
        r,
        "User commands live in [commands.NAME] tables and are bound as \"run NAME\". Theme \
         colours can come from matugen through include = [...] or from pywal with \
         [theme] pywal = \"~/.cache/wal/colors.json\".",
    );

    section(r, "IPC");
    para(
        r,
        "Each instance listens on $XDG_RUNTIME_DIR/remage/PID.sock for newline-delimited JSON: \
         {\"action\":\"goto 5\"}, {\"query\":\"current\"} (or marked, queue, state) and \
         {\"subscribe\":[\"image\",\"marks\",\"mode\"]}. remage msg is a client for it.",
    );

    section(r, "ENVIRONMENT");
    for (name, desc) in [
        (
            "REMAGE_SOCKET",
            "The socket remage msg talks to. Set for user commands.",
        ),
        (
            "REMAGE_FILE, REMAGE_INDEX, REMAGE_COUNT, REMAGE_MODE, REMAGE_PID",
            "Set for user commands: the current file, its 1-based index, the number of targets, \
             the mode and the viewer's pid.",
        ),
        (
            "XDG_CONFIG_HOME, XDG_CACHE_HOME, XDG_RUNTIME_DIR",
            "Where the config, the shared thumbnail cache and the IPC sockets live.",
        ),
        (
            "WGPU_BACKEND, WGPU_POWER_PREF",
            "Override the GPU backend (vulkan, gl) and power preference (low, high).",
        ),
        (
            "RUST_LOG",
            "Logging, e.g. remage=debug for startup and first-frame timings.",
        ),
    ] {
        item(r, vec![bold(name)], desc);
    }

    section(r, "SIGNALS");
    item(r, vec![bold("SIGUSR1")], "Reload the config.");

    section(r, "FILES");
    for (name, desc) in [
        ("~/.config/remage/config.toml", "The config file."),
        (
            "~/.cache/thumbnails/",
            "The freedesktop thumbnail cache, shared with file managers.",
        ),
        (
            "/usr/share/doc/remage/config.example.toml",
            "An example config with user commands.",
        ),
    ] {
        item(r, vec![bold(name)], desc);
    }

    section(r, "EXAMPLES");
    for (cmd, desc) in [
        ("remage", "Open the current directory."),
        (
            "remage photo.jpg",
            "Open photo.jpg with the rest of its directory.",
        ),
        ("remage -rt ~/Pictures", "Browse a tree in the grid."),
        (
            "find . -name '*.png' | remage -i -o",
            "Pick images from a list; print the marked ones on exit.",
        ),
        ("remage msg next", "Advance the newest instance."),
        (
            "remage msg --subscribe image",
            "Print a JSON line whenever the image changes.",
        ),
    ] {
        item(r, vec![bold(cmd)], desc);
    }

    section(r, "SEE ALSO");
    r.text([roman("nsxiv(1), matugen(1), wal(1)")]);
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_keys_and_actions() {
        let mut out = Vec::new();
        super::render(&mut out).unwrap();
        let page = String::from_utf8(out).unwrap();
        for needle in [".SH KEYS", ".SH ACTIONS", "slideshow\\-delay", "Next image"] {
            assert!(page.contains(needle), "missing {needle}");
        }
    }
}

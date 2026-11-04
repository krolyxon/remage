//! `[[decoders]]`: external programs that turn anything else into PNG, PPM or TIFF on stdout.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::RwLock;

use anyhow::{Context, anyhow};

use super::{DecodedImage, Options, image_rs};
use crate::config::Decoder;

static DECODERS: RwLock<Vec<Decoder>> = RwLock::new(Vec::new());

/// Install the configured decoders (at startup and on every config reload).
pub fn set_decoders(decoders: &[Decoder]) {
    let mut d = DECODERS.write().unwrap_or_else(|e| e.into_inner());
    *d = decoders.to_vec();
}

/// The first decoder whose patterns match `path`.
pub fn find(path: &Path) -> Option<Decoder> {
    let d = DECODERS.read().unwrap_or_else(|e| e.into_inner());
    if d.is_empty() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy();
    let full = path.to_string_lossy();
    d.iter()
        .find(|dec| {
            dec.patterns.iter().any(|p| {
                // Patterns with a slash match the whole path, others the file name.
                let target = if p.contains('/') { &*full } else { &*name };
                glob(&p.to_lowercase(), &target.to_lowercase())
            })
        })
        .cloned()
}

/// `*` and `?` wildcards.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

pub fn decode(path: &Path, d: &Decoder, opts: &Options) -> anyhow::Result<DecodedImage> {
    let file = path.as_os_str();
    let mut args = d.run.iter().map(|a| {
        let a = match (a.strip_prefix("~/"), std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => format!("{}/{rest}", home.to_string_lossy()),
            _ => a.clone(),
        };
        // `{file}` stands for the path, also inside a larger argument (`{file}[0]`).
        match a.split_once("{file}") {
            Some((pre, post)) => {
                let mut s = std::ffi::OsString::from(pre);
                s.push(file);
                s.push(post);
                s
            }
            None => a.into(),
        }
    });
    let program = args
        .next()
        .ok_or_else(|| anyhow!("empty decoder command"))?;
    let mut child = Command::new(&program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| program.to_string_lossy().into_owned())?;
    let mut out = Vec::new();
    if let Some(mut s) = child.stdout.take() {
        s.read_to_end(&mut out)?;
    }
    let mut err = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    let status = child.wait()?;
    if !status.success() {
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(anyhow!(
            "{} failed: {status} {first}",
            program.to_string_lossy()
        ));
    }
    let mut img = image_rs::decode_bytes(&out, opts)?;
    img.format = "EXT";
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globbing() {
        assert!(glob("*.psd", "a.psd"));
        assert!(glob("*.psd", ".psd"));
        assert!(!glob("*.psd", "a.psd.bak"));
        assert!(glob("img-??.x*", "img-01.xcf"));
        assert!(glob("*a*b*", "xaxxbx"));
        assert!(!glob("a?", "a"));
    }

    #[test]
    fn decodes_through_a_command() {
        let dir = std::env::temp_dir().join(format!("remage-ext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("src.png");
        image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]))
            .save(&png)
            .unwrap();
        let fake = dir.join("pic.fake");
        std::fs::copy(&png, &fake).unwrap();
        let d = Decoder {
            patterns: vec!["*.FAKE".into()],
            run: vec!["cat".into(), "{file}".into()],
        };
        set_decoders(std::slice::from_ref(&d));
        assert!(find(&fake).is_some());
        assert!(find(&png).is_none());
        let img = decode(&fake, &d, &Options::default()).unwrap();
        assert_eq!((img.size, img.format), ([3, 2], "EXT"));
        let bad = Decoder {
            patterns: vec![],
            run: vec!["sh".into(), "-c".into(), "echo nope >&2; exit 2".into()],
        };
        let e = decode(&fake, &bad, &Options::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("nope"), "{e}");
        set_decoders(&[]);
    }
}

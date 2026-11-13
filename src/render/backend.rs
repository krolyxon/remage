//! Remembers which wgpu backend won the adapter search, so the next start initialises only that
//! one. Probing Vulkan and GL together costs tens of milliseconds before the first frame.

use std::path::PathBuf;

/// The backend and adapter of the last full probe.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub backend: wgpu::Backend,
    pub adapter: String,
}

impl Choice {
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let backend = match lines.next()? {
            "vulkan" => wgpu::Backend::Vulkan,
            "gl" => wgpu::Backend::Gl,
            _ => return None,
        };
        let adapter = lines.next().filter(|a| !a.is_empty())?.to_string();
        Some(Self { backend, adapter })
    }

    fn encode(&self) -> Option<String> {
        let backend = match self.backend {
            wgpu::Backend::Vulkan => "vulkan",
            wgpu::Backend::Gl => "gl",
            _ => return None,
        };
        Some(format!("{backend}\n{}\n", self.adapter))
    }
}

fn path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("remage").join("backend"))
}

/// Whether the environment steers the adapter search; the choice is then neither used nor kept.
pub fn steered() -> bool {
    ["WGPU_BACKEND", "WGPU_POWER_PREF", "WGPU_ADAPTER_NAME"]
        .iter()
        .any(|v| std::env::var_os(v).is_some())
}

pub fn load() -> Option<Choice> {
    if steered() {
        return None;
    }
    Choice::parse(&std::fs::read_to_string(path()?).ok()?)
}

/// Remember `choice` (or forget the old one, for a backend that can't be asked for alone).
pub fn store(choice: &Choice) {
    let Some(path) = path() else { return };
    let Some(text) = choice.encode() else {
        let _ = std::fs::remove_file(&path);
        return;
    };
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        log::debug!("backend cache not written: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let c = Choice {
            backend: wgpu::Backend::Gl,
            adapter: "Mesa Intel(R) Graphics (RPL-S)".into(),
        };
        assert_eq!(Choice::parse(&c.encode().unwrap()), Some(c));
        assert_eq!(
            Choice::parse("vulkan\nNVIDIA\n").map(|c| c.backend),
            Some(wgpu::Backend::Vulkan)
        );
        for bad in ["", "gl", "gl\n", "metal\nApple M1\n", "garbage"] {
            assert_eq!(Choice::parse(bad), None, "{bad:?}");
        }
        let other = Choice {
            backend: wgpu::Backend::Metal,
            adapter: "x".into(),
        };
        assert_eq!(other.encode(), None);
    }
}

//! File and EXIF metadata for the info panel.

use std::path::Path;
use std::time::SystemTime;

use exif::{In, Tag};

/// Label/value rows describing `path` (the UI adds the decoded format and size). Never fails: missing data just means fewer rows.
pub fn read(path: &Path) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut push = |l: &str, v: String| {
        if !v.trim().is_empty() {
            rows.push((l.to_string(), v));
        }
    };
    push("File", crate::state::file_name(path));
    if let Some(d) = path.parent() {
        push("Folder", d.display().to_string());
    }
    if let Ok(m) = std::fs::metadata(path) {
        push("Size", human_size(m.len()));
        if let Ok(t) = m.modified() {
            push("Modified", datetime(t));
        }
    }
    if let Some(exif) = std::fs::File::open(path)
        .ok()
        .map(std::io::BufReader::new)
        .and_then(|mut r| exif::Reader::new().read_from_container(&mut r).ok())
    {
        let field = |t: Tag| {
            exif.get_field(t, In::PRIMARY)
                .map(|f| f.display_value().with_unit(&exif).to_string())
                .map(|s| s.trim_matches('"').trim().to_string())
        };
        let make = field(Tag::Make).unwrap_or_default();
        let model = field(Tag::Model).unwrap_or_default();
        // Most models already start with the make ("Canon EOS R6").
        let camera = if model.to_lowercase().starts_with(&make.to_lowercase()) {
            model
        } else {
            format!("{make} {model}")
        };
        push("Camera", camera);
        push("Lens", field(Tag::LensModel).unwrap_or_default());
        push(
            "Taken",
            field(Tag::DateTimeOriginal)
                .or_else(|| field(Tag::DateTime))
                .unwrap_or_default(),
        );
        let exposure = [
            field(Tag::ExposureTime),
            field(Tag::FNumber),
            field(Tag::PhotographicSensitivity).map(|i| format!("ISO {i}")),
            field(Tag::FocalLength),
        ];
        push(
            "Exposure",
            exposure
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("  "),
        );
        push("Flash", field(Tag::Flash).unwrap_or_default());
        push("Software", field(Tag::Software).unwrap_or_default());
        push("Artist", field(Tag::Artist).unwrap_or_default());
        push("Copyright", field(Tag::Copyright).unwrap_or_default());
        if let (Some(lat), Some(lon)) = (
            gps(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef),
            gps(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef),
        ) {
            push("GPS", format!("{lat:.5}, {lon:.5}"));
        }
    }
    rows
}

/// A GPS coordinate in signed decimal degrees.
fn gps(exif: &exif::Exif, tag: Tag, reference: Tag) -> Option<f64> {
    let f = exif.get_field(tag, In::PRIMARY)?;
    let exif::Value::Rational(v) = &f.value else {
        return None;
    };
    let deg = v.first()?.to_f64()
        + v.get(1).map_or(0.0, |m| m.to_f64() / 60.0)
        + v.get(2).map_or(0.0, |s| s.to_f64() / 3600.0);
    let neg = exif
        .get_field(reference, In::PRIMARY)
        .map(|r| r.display_value().to_string())
        .is_some_and(|r| r.contains('S') || r.contains('W'));
    Some(if neg { -deg } else { deg })
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// `YYYY-MM-DD HH:MM` in UTC (no time zone database needed).
fn datetime(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_dates() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 << 20), "5.0 MB");
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19_723), (2024, 1, 1));
        assert_eq!(civil(11_016), (2000, 2, 29));
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(datetime(t), "2023-11-14 22:13 UTC");
    }

    #[test]
    fn reads_fixture_metadata() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let Some(f) = std::fs::read_dir(&dir).ok().and_then(|mut d| {
            d.find_map(|e| {
                let p = e.ok()?.path();
                (p.extension()? == "png").then_some(p)
            })
        }) else {
            return;
        };
        let rows = read(&f);
        assert_eq!(rows[0].0, "File");
        assert!(rows.iter().any(|(l, _)| l == "Size"), "{rows:?}");
    }
}

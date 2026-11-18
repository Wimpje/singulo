use crate::model::{Media, Result, Settings};
use chrono::{Datelike, NaiveDateTime};
use std::path::{Component, Path, PathBuf};

pub fn component(value: &str) -> Result<()> {
    let upper = value.split('.').next().unwrap_or("").to_uppercase();
    if value.is_empty()
        || value.len() > 180
        || value == "."
        || value == ".."
        || value.eq_ignore_ascii_case(".singulo")
        || value.ends_with([' ', '.'])
        || value
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*{}".contains(c))
        || matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && matches!(upper.as_bytes()[3], b'1'..=b'9')
    {
        return Err(format!(
            "Invalid cross-platform folder or filename: {value}"
        ));
    }
    Ok(())
}

pub fn validate(settings: &Settings) -> Result<()> {
    if !matches!(settings.filenames.as_str(), "original" | "date") {
        return Err("Choose original or date filenames.".into());
    }
    component(&settings.unknown_folder)?;
    if settings.template.is_empty() || settings.template.len() > 300 {
        return Err("Folder layout must be between 1 and 300 characters.".into());
    }
    let mut sample = settings.template.clone();
    for (token, value) in [
        ("{yyyy}", "2023"),
        ("{mm}", "07"),
        ("{dd}", "18"),
        ("{media_type}", "Photos"),
    ] {
        sample = sample.replace(token, value);
    }
    for part in sample.split('/') {
        component(part)?;
    }
    Ok(())
}

pub fn safe_name(original: &str) -> String {
    let mut name: String = original
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*{}".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(150)
        .collect();
    name = name.trim_end_matches([' ', '.']).to_string();
    if component(&name).is_err() {
        name = format!("photo_{}", name.trim_matches('.'));
    }
    name
}

pub fn relative(settings: &Settings, media: &Media, original: &str) -> Result<PathBuf> {
    validate(settings)?;
    let category = if media.kind == "video" {
        "Videos"
    } else {
        "Photos"
    };
    let date = media.date.as_deref();
    let (folder, basename) = if let Some(date) = date {
        let day = chrono::NaiveDate::parse_from_str(
            date.get(..10).ok_or("Selected date is incomplete.")?,
            "%Y-%m-%d",
        )
        .map_err(|e| format!("Invalid selected date: {e}"))?;
        let folder = settings
            .template
            .replace("{yyyy}", &format!("{:04}", day.year()))
            .replace("{mm}", &format!("{:02}", day.month()))
            .replace("{dd}", &format!("{:02}", day.day()))
            .replace("{media_type}", category);
        let name = if settings.filenames == "date" {
            let stem = if date.len() > 10 {
                NaiveDateTime::parse_from_str(date, "%Y-%m-%dT%H:%M:%S")
                    .map_err(|e| format!("Invalid selected time: {e}"))?
                    .format("%Y-%m-%d_%H-%M-%S")
                    .to_string()
            } else {
                date.into()
            };
            match Path::new(original).extension().and_then(|x| x.to_str()) {
                Some(ext) => format!("{stem}.{ext}"),
                None => stem,
            }
        } else {
            safe_name(original)
        };
        (folder, name)
    } else {
        let folder = if settings.template.starts_with("{media_type}/") {
            format!("{category}/{}", settings.unknown_folder)
        } else {
            settings.unknown_folder.clone()
        };
        (folder, safe_name(original))
    };
    let path = PathBuf::from(folder).join(basename);
    for part in path.components() {
        match part {
            Component::Normal(part) => component(&part.to_string_lossy())?,
            _ => return Err("Output paths must stay inside the archive.".into()),
        }
    }
    Ok(path)
}

pub fn canonical(path: &Path) -> Result<PathBuf> {
    let real = path.canonicalize().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    if let Some(text) = real.to_str()
        && let Some(stripped) = text.strip_prefix(r"\\?\")
        && !stripped.starts_with(r"UNC\")
    {
        return Ok(PathBuf::from(stripped));
    }
    Ok(real)
}

pub fn suffixed(wanted: &Path, id: &str) -> Result<PathBuf> {
    let stem = wanted
        .file_stem()
        .ok_or("Missing filename stem.")?
        .to_string_lossy();
    let ext = wanted
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    Ok(wanted.with_file_name(format!("{stem}__{id}{ext}")))
}

pub fn inside(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err("Unsafe archive-relative path.".into());
    }
    let target = root.join(relative);
    let mut parent = target.parent();
    while let Some(path) = parent {
        if path == root {
            break;
        }
        if path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err("An archive path escapes through a symlink.".into());
        }
        parent = path.parent();
    }
    if target
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err("Refusing to use a symlink as an archived file.".into());
    }
    let text = target.to_string_lossy();
    let portable = text.strip_prefix(r"\\?\").unwrap_or(&text);
    if portable.encode_utf16().count() > 240 {
        return Err(format!(
            "Output path is too long for portable Windows support: {}",
            target.display()
        ));
    }
    Ok(target)
}

pub fn unique_for(
    root: &Path,
    wanted: &Path,
    id: &str,
    current: Option<&Path>,
) -> Result<PathBuf> {
    let target = inside(root, wanted)?;
    let current_id = current
        .and_then(|rel| inside(root, rel).ok())
        .and_then(|abs| crate::media::identity(&abs).ok());
    let is_self = |candidate: &Path| -> bool {
        current_id
            .as_ref()
            .is_some_and(|cid| crate::media::identity(candidate).ok().as_ref() == Some(cid))
    };
    let parent = target.parent().ok_or("Missing output parent.")?;
    let name = target
        .file_name()
        .ok_or("Missing filename.")?
        .to_string_lossy();
    let exists = parent.exists()
        && std::fs::read_dir(parent)
            .map_err(|e| e.to_string())?
            .any(|entry| {
                entry.is_ok_and(|e| {
                    e.file_name().to_string_lossy().eq_ignore_ascii_case(&name)
                        && !is_self(&e.path())
                })
            });
    if !exists {
        return Ok(wanted.to_path_buf());
    }
    let result = suffixed(wanted, id)?;
    let result_target = inside(root, &result)?;
    if result_target.exists() && !is_self(&result_target) {
        return Err("Collision at the generated unique filename.".into());
    }
    Ok(result)
}

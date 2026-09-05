// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use rune_cfg::RuneConfig;

#[derive(Debug, PartialEq, Eq)]
pub enum MigrateOutcome {
    Current,
    Migrated {
        backup_path: PathBuf,
        changes: Vec<String>,
    },
}

/// Compatibility fields introduced after the current `default:` format.
///
/// Keep this list deliberately narrow. Optional settings continue to use their
/// parser defaults and are not written into a user's customized file merely
/// because the shipped example contains them.
const DEFAULT_BACKFILLS: &[(&str, &str)] = &[
    ("suspend_inhibit_media", "suspend_inhibit_media [ ]"),
    ("suspend_inhibit_apps", "suspend_inhibit_apps [ ]"),
];

/// Upgrade a config without replacing its structure, comments, or values.
///
/// A content backup is created only when a rewrite is needed. Writes are
/// validated first and atomically replace the real file. When `path` is a
/// symlink, the target is replaced while the symlink itself remains untouched.
pub fn migrate_in_place(path: &Path) -> Result<MigrateOutcome, String> {
    let original = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;

    let (mut rewritten, mut changes, inherited_loginctl) = rewrite_legacy_keys(&original);
    let parsed = parse_and_validate(&rewritten)?;

    let mut additions = Vec::new();
    if inherited_loginctl && !parsed.has("default.enable_loginctl_integration") {
        additions.push("enable_loginctl_integration true".to_string());
        changes.push("added default.enable_loginctl_integration".to_string());
    }

    for (key, line) in DEFAULT_BACKFILLS {
        let path = format!("default.{key}");
        if !parsed.has(&path) {
            additions.push((*line).to_string());
            changes.push(format!("added {path}"));
        }
    }

    if !additions.is_empty() {
        rewritten = insert_default_fields(&rewritten, &additions)?;
    }

    if changes.is_empty() {
        return Ok(MigrateOutcome::Current);
    }

    // Validate the final text with both Rune and Stasis before touching disk.
    let _ = parse_and_validate(&rewritten)?;

    let backup_path = next_backup_name(path);
    fs::copy(path, &backup_path).map_err(|e| {
        format!(
            "backup copy {} -> {}: {e}",
            path.display(),
            backup_path.display()
        )
    })?;

    write_atomic_preserving_symlink(path, rewritten.as_bytes())?;

    Ok(MigrateOutcome::Migrated {
        backup_path,
        changes,
    })
}

fn parse_and_validate(text: &str) -> Result<RuneConfig, String> {
    let config = RuneConfig::from_str(text)
        .map_err(|e| format!("configuration is invalid and was left unchanged: {e}"))?;
    super::parse_config_file(&config)
        .map_err(|e| format!("configuration is invalid and was left unchanged: {e}"))?;
    Ok(config)
}

fn rewrite_legacy_keys(text: &str) -> (String, Vec<String>, bool) {
    let had_trailing_newline = text.ends_with('\n');
    let mut lines = Vec::new();
    let mut changes = Vec::new();
    let mut inherited_loginctl = false;

    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];

        if let Some(value) = trimmed.strip_prefix("listen_browser_dbus_inhibit ") {
            lines.push(format!("{indent}enable_dbus_inhibit {value}"));
            changes.push("renamed listen_browser_dbus_inhibit to enable_dbus_inhibit".to_string());
        } else if let Some(value) = trimmed.strip_prefix("enable_loginctl ") {
            lines.push(format!("{indent}enable_loginctl_integration {value}"));
            changes.push("renamed enable_loginctl to enable_loginctl_integration".to_string());
        } else if let Some(value) = trimmed.strip_prefix("use_loginctl ") {
            inherited_loginctl |= value
                .split_whitespace()
                .next()
                .is_some_and(|value| value.eq_ignore_ascii_case("true"));
            changes.push("removed legacy per-step use_loginctl".to_string());
        } else {
            lines.push(line.to_string());
        }
    }

    let mut rewritten = lines.join("\n");
    if had_trailing_newline {
        rewritten.push('\n');
    }
    (rewritten, changes, inherited_loginctl)
}

fn insert_default_fields(text: &str, additions: &[String]) -> Result<String, String> {
    let had_trailing_newline = text.ends_with('\n');
    let mut out = Vec::new();
    let mut inserted = false;

    for line in text.lines() {
        out.push(line.to_string());
        if !inserted && line.trim() == "default:" {
            let parent_indent = &line[..line.len() - line.trim_start().len()];
            let field_indent = format!("{parent_indent}  ");
            out.push(format!(
                "{field_indent}# Added by Stasis for configuration compatibility."
            ));
            out.extend(
                additions
                    .iter()
                    .map(|addition| format!("{field_indent}{addition}")),
            );
            inserted = true;
        }
    }

    if !inserted {
        return Err("configuration has no `default:` block and was left unchanged".to_string());
    }

    let mut rewritten = out.join("\n");
    if had_trailing_newline {
        rewritten.push('\n');
    }
    Ok(rewritten)
}

fn write_atomic_preserving_symlink(path: &Path, contents: &[u8]) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|e| format!("metadata {}: {e}", path.display()))?;
    let target = if fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        fs::canonicalize(path).map_err(|e| format!("resolve symlink {}: {e}", path.display()))?
    } else {
        path.to_path_buf()
    };

    let parent = target
        .parent()
        .ok_or_else(|| format!("configuration path has no parent: {}", target.display()))?;
    let filename = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("stasis.rune");

    for sequence in 0..100u32 {
        let temporary = parent.join(format!(
            ".{filename}.stasis-migrate-{}-{sequence}.tmp",
            std::process::id()
        ));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("create {}: {error}", temporary.display())),
        };

        let write_result = (|| -> std::io::Result<()> {
            file.write_all(contents)?;
            file.sync_all()?;
            fs::set_permissions(&temporary, metadata.permissions())?;
            fs::rename(&temporary, &target)?;
            Ok(())
        })();

        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("write migrated {}: {error}", target.display()));
        }
        return Ok(());
    }

    Err(format!(
        "could not reserve a temporary file beside {}",
        target.display()
    ))
}

fn next_backup_name(path: &Path) -> PathBuf {
    let first = PathBuf::from(format!("{}.bak", path.display()));
    if !first.exists() {
        return first;
    }

    for number in 1u64.. {
        let candidate = PathBuf::from(format!("{}.bak.{number}", path.display()));
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("the backup suffix counter is unbounded")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "stasis-migrate-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp directory should be created");
        path
    }

    #[test]
    fn current_bootstrap_needs_no_migration() {
        let dir = temp_dir("current");
        let path = dir.join("stasis.rune");
        let text = super::super::bootstrap::default_config_contents();
        fs::write(&path, &text).unwrap();

        assert_eq!(migrate_in_place(&path).unwrap(), MigrateOutcome::Current);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert!(!dir.join("stasis.rune.bak").exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backfill_preserves_custom_values_comments_and_numbered_backups() {
        let dir = temp_dir("backfill");
        let path = dir.join("stasis.rune");
        let original = r#"# keep this comment
default:
  monitor_media false # user's choice
  ignore_remote_media false
  inhibit_apps ["custom-app"]
end
"#;
        fs::write(&path, original).unwrap();
        fs::write(dir.join("stasis.rune.bak"), "older backup").unwrap();

        let outcome = migrate_in_place(&path).unwrap();
        let MigrateOutcome::Migrated {
            backup_path,
            changes,
        } = outcome
        else {
            panic!("missing compatibility fields should migrate");
        };

        let migrated = fs::read_to_string(&path).unwrap();
        assert!(migrated.contains("# keep this comment"));
        assert!(migrated.contains("monitor_media false # user's choice"));
        assert!(migrated.contains("inhibit_apps [\"custom-app\"]"));
        assert!(migrated.contains("suspend_inhibit_media [ ]"));
        assert!(migrated.contains("suspend_inhibit_apps [ ]"));
        assert_eq!(backup_path, dir.join("stasis.rune.bak.1"));
        assert_eq!(fs::read_to_string(backup_path).unwrap(), original);
        assert_eq!(changes.len(), 2);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_config_is_left_untouched_without_a_backup() {
        let dir = temp_dir("invalid");
        let path = dir.join("stasis.rune");
        let original = "default:\n  broken [\nend\n";
        fs::write(&path, original).unwrap();

        let error = migrate_in_place(&path).unwrap_err();
        assert!(error.contains("left unchanged"));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(!dir.join("stasis.rune.bak").exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_keys_are_renamed_without_replacing_other_content() {
        let dir = temp_dir("legacy-keys");
        let path = dir.join("stasis.rune");
        let original = r#"default:
  enable_loginctl false
  listen_browser_dbus_inhibit false
  monitor_media false
  ignore_remote_media false
  inhibit_apps [ ]
  suspend_inhibit_media [ ]
  suspend_inhibit_apps [ ]
end
"#;
        fs::write(&path, original).unwrap();

        migrate_in_place(&path).unwrap();
        let migrated = fs::read_to_string(&path).unwrap();
        assert!(migrated.contains("enable_loginctl_integration false"));
        assert!(migrated.contains("enable_dbus_inhibit false"));
        assert!(!migrated.contains("listen_browser_dbus_inhibit"));

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_target_location_survive_backfill() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir("symlink");
        let target_dir = dir.join("managed");
        fs::create_dir(&target_dir).unwrap();
        let target = target_dir.join("real.rune");
        let link = dir.join("stasis.rune");
        let original = r#"default:
  monitor_media false
  ignore_remote_media true
  inhibit_apps ["vlc"]
end
"#;
        fs::write(&target, original).unwrap();
        symlink(Path::new("managed/real.rune"), &link).unwrap();

        let outcome = migrate_in_place(&link).unwrap();
        assert!(matches!(outcome, MigrateOutcome::Migrated { .. }));
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(&link).unwrap(),
            Path::new("managed/real.rune")
        );
        let migrated = fs::read_to_string(&target).unwrap();
        assert!(migrated.contains("monitor_media false"));
        assert!(migrated.contains("suspend_inhibit_media [ ]"));
        assert!(migrated.contains("suspend_inhibit_apps [ ]"));
        assert_eq!(
            fs::read_to_string(dir.join("stasis.rune.bak")).unwrap(),
            original
        );

        fs::remove_dir_all(dir).unwrap();
    }
}

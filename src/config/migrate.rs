// Author: Dustin Pilgrim
// License: GPL-3.0-only

use std::fs;
use std::path::{Path, PathBuf};

use rune_cfg::RuneConfig;

#[derive(Debug)]
pub enum MigrateOutcome {
    Current,
    Replaced {
        backup_path: PathBuf,
        reason: String,
    },
}

/// Replace obsolete or invalid configurations with the current bootstrap.
///
/// Migration deliberately does not rewrite individual fields. The previous
/// file is preserved verbatim and bootstrap remains the sole source of newly
/// generated configuration content.
pub fn migrate_in_place(path: &Path) -> Result<MigrateOutcome, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;

    let Some(reason) = replacement_reason(&text)? else {
        return Ok(MigrateOutcome::Current);
    };

    let backup_path = next_backup_name(path);
    fs::rename(path, &backup_path).map_err(|e| {
        format!(
            "backup rename {} -> {}: {e}",
            path.display(),
            backup_path.display()
        )
    })?;

    if let Err(write_error) = super::bootstrap::write_default_config(path) {
        // The original is still safe in the backup. Best effort restores its
        // original name so a generation failure does not leave Stasis configless.
        let _ = fs::remove_file(path);
        let restore_result = fs::rename(&backup_path, path);
        return match restore_result {
            Ok(()) => Err(format!(
                "write replacement {}: {write_error}; original restored",
                path.display()
            )),
            Err(restore_error) => Err(format!(
                "write replacement {}: {write_error}; original remains at {} (restore failed: {restore_error})",
                path.display(),
                backup_path.display()
            )),
        };
    }

    Ok(MigrateOutcome::Replaced {
        backup_path,
        reason,
    })
}

fn replacement_reason(text: &str) -> Result<Option<String>, String> {
    let config = match RuneConfig::from_str(text) {
        Ok(config) => config,
        Err(error) => return Ok(Some(format!("invalid Rune syntax: {error}"))),
    };

    if let Err(error) = super::parse_config_file(&config) {
        return Ok(Some(format!("invalid Stasis configuration: {error}")));
    }

    let reference_text = super::bootstrap::default_config_contents();
    let reference = RuneConfig::from_str(&reference_text)
        .map_err(|e| format!("internal bootstrap config is invalid: {e}"))?;

    for key in reference.get_keys("default").unwrap_or_default() {
        let path = format!("default.{key}");

        // Blocks are plan steps/containers, not global knobs. A current custom
        // plan may intentionally use completely different steps.
        if !reference.get_keys(&path).unwrap_or_default().is_empty() {
            continue;
        }

        if !config.has(&path) {
            return Ok(Some(format!("missing configuration knob `{path}`")));
        }
    }

    Ok(None)
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
        let text = super::super::bootstrap::default_config_contents();
        assert_eq!(replacement_reason(&text).unwrap(), None);
    }

    #[test]
    fn missing_knob_and_invalid_config_need_replacement() {
        let current = super::super::bootstrap::default_config_contents();
        let missing = current
            .lines()
            .filter(|line| !line.trim_start().starts_with("suspend_inhibit_media "))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            replacement_reason(&missing)
                .unwrap()
                .expect("missing knob should be detected")
                .contains("suspend_inhibit_media")
        );
        assert!(
            replacement_reason("default:\n  broken [\nend\n")
                .unwrap()
                .expect("invalid Rune should be detected")
                .contains("invalid Rune syntax")
        );
    }

    #[test]
    fn replacement_preserves_numbered_backups_and_uses_bootstrap() {
        let dir = temp_dir("numbered");
        let path = dir.join("stasis.rune");
        fs::write(&path, "not valid rune [").unwrap();
        fs::write(dir.join("stasis.rune.bak"), "older backup").unwrap();
        fs::write(dir.join("stasis.rune.bak.1"), "newer backup").unwrap();

        let outcome = migrate_in_place(&path).expect("migration should succeed");
        let MigrateOutcome::Replaced { backup_path, .. } = outcome else {
            panic!("invalid config should be replaced");
        };

        assert_eq!(backup_path, dir.join("stasis.rune.bak.2"));
        assert_eq!(
            fs::read_to_string(&backup_path).unwrap(),
            "not valid rune ["
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            super::super::bootstrap::default_config_contents()
        );
        assert_eq!(
            fs::read_to_string(dir.join("stasis.rune.bak")).unwrap(),
            "older backup"
        );
        assert_eq!(
            fs::read_to_string(dir.join("stasis.rune.bak.1")).unwrap(),
            "newer backup"
        );

        fs::remove_dir_all(dir).unwrap();
    }
}

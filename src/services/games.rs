// Author: Dustin Pilgrim
// License: GPL-3.0-only

//! Installed-game discovery plus runtime evidence. Discovery never launches a game.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lib_game_detector::{data::Game, get_detector};
use tokio::sync::{mpsc, watch};

use crate::core::{
    config::Pattern,
    events::Event,
    info::{DetectedGame, GamesInfo},
    manager_msg::ManagerMsg,
};

#[derive(Debug, Clone)]
pub struct GameRules {
    pub epoch: u64,
    pub monitor_games: bool,
    pub blacklist: Vec<Pattern>,
    pub extra_games: Vec<Pattern>,
}

#[derive(Debug, Clone)]
struct CatalogueGame {
    identity: DetectedGame,
    directory: Option<PathBuf>,
    steam_app_id: Option<String>,
}

impl CatalogueGame {
    fn from_game(game: Game) -> Self {
        // Command is metadata only. Inspect its arguments; never execute it or
        // parse its shell/debug representation to launch anything.
        let steam_app_id = game
            .launch_command
            .get_args()
            .filter_map(|arg| arg.to_str())
            .find_map(|arg| arg.strip_prefix("steam://rungameid/"))
            .filter(|id| is_app_id(id))
            .map(str::to_owned);
        let source = game.source.to_string();
        let id = steam_app_id
            .as_ref()
            .map(|id| format!("steam:{id}"))
            .unwrap_or_else(|| format!("{source}:{}:{:?}", game.title, game.launch_command));
        let directory = game.path_game_dir.map(|path| canonical_or_original(&path));
        Self {
            identity: DetectedGame {
                id,
                title: game.title,
                source,
                path_game_dir: directory
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                pids: Vec::new(),
                extra_rule: None,
            },
            directory,
            steam_app_id,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Catalogue {
    by_launcher: BTreeMap<String, Vec<CatalogueGame>>,
    errors: Vec<String>,
}

impl Catalogue {
    fn refresh(mut self) -> Self {
        let detector = get_detector();
        let launchers = detector.get_detected_launchers();
        let mut present = BTreeSet::new();
        self.errors.clear();
        for launcher in launchers {
            let source = launcher.get_launcher_type().to_string();
            present.insert(source.clone());
            match launcher.get_detected_games() {
                Ok(games) => {
                    self.by_launcher.insert(
                        source,
                        games.into_iter().map(CatalogueGame::from_game).collect(),
                    );
                }
                Err(error) => {
                    // Keep only this launcher's last successful catalogue. A
                    // broken launcher must not discard other launchers' games.
                    self.errors.push(format!("{source}: {error}"));
                }
            }
        }
        self.by_launcher
            .retain(|source, _| present.contains(source));
        self
    }

    fn games(&self) -> impl Iterator<Item = &CatalogueGame> {
        self.by_launcher.values().flatten()
    }

    fn detect(&self, processes: &[ObservedProcess], window_ids: &[String]) -> Vec<DetectedGame> {
        let mut running = BTreeMap::<String, DetectedGame>::new();
        for process in processes {
            let mut best: Option<&CatalogueGame> = None;
            let mut best_score = (false, 0);
            let mut ambiguous = false;
            for game in self.games().filter(|game| process.matches(game)) {
                let score = (
                    process.wine_game
                        && process
                            .steam_app_id
                            .as_ref()
                            .is_some_and(|id| game.steam_app_id.as_ref() == Some(id)),
                    game.directory
                        .as_ref()
                        .map_or(0, |path| path.components().count()),
                );
                if best.is_none() || score > best_score {
                    best = Some(game);
                    best_score = score;
                    ambiguous = false;
                } else if score == best_score
                    && best.is_some_and(|best| best.identity.id != game.identity.id)
                {
                    ambiguous = true;
                }
            }
            // Prefer a specific installation/instance over its parent directory.
            // Shared prefixes with no distinguishing ID are ambiguous: do not
            // guess that every game in that prefix is running.
            if let Some(game) = best.filter(|_| !ambiguous) {
                let observed = running
                    .entry(game.identity.id.clone())
                    .or_insert_with(|| game.identity.clone());
                observed.pids.push(process.pid);
                observed.pids.sort_unstable();
                observed.pids.dedup();
            }
        }
        // A genuine Steam app window remains useful evidence when the crate
        // misses metadata (e.g. no cached box art). Blacklist by Steam ID still
        // works; we do not invent a title or guess from the window title.
        for app_id in window_ids {
            let id = format!("steam:{app_id}");
            running.entry(id.clone()).or_insert_with(|| {
                self.games()
                    .find(|game| game.steam_app_id.as_ref() == Some(app_id))
                    .map(|game| game.identity.clone())
                    .unwrap_or_else(|| DetectedGame {
                        id,
                        title: format!("Steam app {app_id}"),
                        source: "Steam".into(),
                        path_game_dir: None,
                        pids: Vec::new(),
                        extra_rule: None,
                    })
            });
        }
        running.into_values().collect()
    }
}

fn extra_window_matches(pattern: &Pattern, app_id: &str) -> bool {
    let app_id = app_id.to_lowercase();
    match pattern {
        Pattern::Literal(literal) => {
            app_id == *literal || app_id.strip_suffix(".exe") == Some(literal.as_str())
        }
        Pattern::Regex(regex) => regex.is_match(&app_id),
    }
}

fn add_extra_games(
    running: &mut Vec<DetectedGame>,
    extra_games: &[Pattern],
    processes: &[ObservedProcess],
    window_app_ids: &[String],
) {
    let auto_pids = running
        .iter()
        .flat_map(|game| game.pids.iter().copied())
        .collect::<BTreeSet<_>>();
    let auto_ids = running
        .iter()
        .map(|game| game.id.clone())
        .collect::<BTreeSet<_>>();
    let mut extras = BTreeMap::<String, DetectedGame>::new();
    let mut process_rules = BTreeSet::new();
    for process in processes
        .iter()
        .filter(|process| !auto_pids.contains(&process.pid))
    {
        let Some(pattern) = extra_games
            .iter()
            .find(|pattern| process.matches_extra(pattern))
        else {
            continue;
        };
        let directory_rule = match pattern {
            Pattern::Literal(literal) if literal.starts_with('/') => Some(literal.as_str()),
            _ => None,
        };
        let title = directory_rule
            .map(|root| basename_lc(Path::new(root)))
            .or_else(|| process.names.first().cloned())
            .unwrap_or_else(|| pattern.render());
        let id = format!("extra:{}", directory_rule.unwrap_or(&title));
        let observed = extras.entry(id.clone()).or_insert_with(|| DetectedGame {
            id,
            title,
            source: "Extra games".into(),
            path_game_dir: directory_rule.map(str::to_owned).or_else(|| {
                process
                    .executable_paths
                    .first()
                    .and_then(|path| path.parent())
                    .map(|path| path.to_string_lossy().into_owned())
            }),
            pids: Vec::new(),
            extra_rule: Some(pattern.render()),
        });
        observed.pids.push(process.pid);
        observed.pids.sort_unstable();
        observed.pids.dedup();
        process_rules.insert(pattern.render());
    }
    for app_id in window_app_ids {
        if steam_window_id(app_id).is_some_and(|id| auto_ids.contains(&format!("steam:{id}"))) {
            continue;
        }
        let Some(pattern) = extra_games
            .iter()
            .find(|pattern| extra_window_matches(pattern, app_id))
        else {
            continue;
        };
        // This rule already has process evidence; the window is supplemental,
        // not a second hold for the same explicitly configured game.
        if process_rules.contains(&pattern.render()) {
            continue;
        }
        let id = format!("extra:{}", app_id.to_lowercase());
        extras.entry(id.clone()).or_insert_with(|| DetectedGame {
            id,
            title: app_id.clone(),
            source: "Extra games".into(),
            path_game_dir: None,
            pids: Vec::new(),
            extra_rule: Some(pattern.render()),
        });
    }
    running.extend(extras.into_values());
    running.sort_by(|a, b| a.id.cmp(&b.id));
}

#[derive(Debug)]
struct ObservedProcess {
    pid: i32,
    names: Vec<String>,
    executable_paths: Vec<PathBuf>,
    java_paths: Vec<PathBuf>,
    wine_game: bool,
    steam_app_id: Option<String>,
}

impl ObservedProcess {
    fn from_parts(
        pid: i32,
        exe: &Path,
        cwd: Option<&Path>,
        argv: &[String],
        steam_app_id: Option<String>,
        wine_prefix: Option<&Path>,
    ) -> Self {
        let mut process = Self {
            pid,
            names: Vec::new(),
            executable_paths: Vec::new(),
            java_paths: Vec::new(),
            wine_game: false,
            steam_app_id,
        };
        let exe_name = basename_lc(exe);
        let arg0 = argv
            .first()
            .map(|arg| arg.replace('\\', "/"))
            .unwrap_or_default();
        let arg0_name = basename_lc(Path::new(&arg0));
        if is_runtime_helper(&exe_name) || is_runtime_helper(&arg0_name) {
            return process;
        }
        // Generic Wine/Java/interpreter binaries are not themselves games.
        // We need their game executable, script, or instance-specific argument.
        let wine = exe_name.starts_with("wine") || exe_name.ends_with(".exe");
        let java = matches!(
            exe_name.as_str(),
            "java" | "javaw" | "java.exe" | "javaw.exe"
        );
        let interpreter = matches!(
            exe_name.as_str(),
            "bash" | "sh" | "dash" | "zsh" | "python" | "python3" | "python2" | "perl" | "ruby"
        ) || exe_name.starts_with("python3.");
        if !wine && !java && !interpreter {
            process.names.push(exe_name.clone());
            process.executable_paths.push(canonical_or_original(exe));
        }
        if wine {
            let target = argv
                .iter()
                .take(2)
                .find(|arg| arg.to_lowercase().ends_with(".exe"));
            if let Some(target) = target {
                let name = basename_lc(Path::new(&target.replace('\\', "/")));
                if !is_runtime_helper(&name) {
                    process.names.push(name);
                    process.wine_game = true;
                    if let Some(path) = executable_arg_path(target, cwd, wine_prefix) {
                        process.executable_paths.push(path);
                    }
                }
            }
        } else if interpreter {
            // Only the script position, not arbitrary file arguments. Opening
            // a file with an editor or a terminal in the game dir is no proof.
            if let Some(script) = argv.get(1).filter(|arg| !arg.starts_with('-'))
                && let Some(path) = executable_arg_path(script, cwd, None)
            {
                process.executable_paths.push(path);
                process.names.push(basename_lc(Path::new(script)));
            }
        } else if java {
            for (i, arg) in argv.iter().enumerate() {
                let values: Vec<&str> = if arg == "--gameDir" || arg == "-jar" {
                    argv.get(i + 1)
                        .map(|value| vec![value.as_str()])
                        .unwrap_or_default()
                } else if let Some(value) = arg.strip_prefix("-Djava.library.path=") {
                    value.split(':').collect()
                } else {
                    Vec::new()
                };
                for value in values {
                    if let Some(path) = executable_arg_path(value, cwd, None) {
                        process.java_paths.push(path);
                    }
                }
            }
        }
        process
    }

    fn matches_extra(&self, pattern: &Pattern) -> bool {
        match pattern {
            Pattern::Literal(literal) if literal.starts_with('/') => {
                let root = literal.trim_end_matches('/');
                !root.is_empty()
                    && self
                        .executable_paths
                        .iter()
                        .chain(&self.java_paths)
                        .any(|path| {
                            let path = path.to_string_lossy().to_lowercase();
                            path == root
                                || path
                                    .strip_prefix(root)
                                    .is_some_and(|tail| tail.starts_with('/'))
                        })
            }
            Pattern::Literal(literal) => self
                .names
                .iter()
                .any(|name| name == literal || name.strip_suffix(".exe") == Some(literal)),
            Pattern::Regex(regex) => {
                self.names.iter().any(|name| regex.is_match(name))
                    || self
                        .executable_paths
                        .iter()
                        .chain(&self.java_paths)
                        .any(|path| regex.is_match(&path.to_string_lossy().to_lowercase()))
            }
        }
    }

    fn matches(&self, game: &CatalogueGame) -> bool {
        // Proton may use a Windows C: path whose installation path cannot be
        // mapped to Unix. An actual non-helper .exe and its SteamAppId together
        // are evidence; an inherited ID on a launcher/wineserver is not.
        if self.wine_game
            && self
                .steam_app_id
                .as_ref()
                .is_some_and(|id| game.steam_app_id.as_ref() == Some(id))
        {
            return true;
        }
        let Some(root) = &game.directory else {
            return false;
        };
        self.executable_paths
            .iter()
            .any(|path| path != root && path.starts_with(root))
            || self.java_paths.iter().any(|path| path.starts_with(root))
    }
}

fn is_runtime_helper(name: &str) -> bool {
    matches!(
        name,
        "steam"
            | "steamwebhelper"
            | "steamservice"
            | "steam-runtime-launcher-service"
            | "lutris"
            | "heroic"
            | "bottles"
            | "bottles-cli"
            | "prismlauncher"
            | "atlauncher"
            | "wineserver"
            | "wineboot.exe"
            | "services.exe"
            | "explorer.exe"
            | "winedevice.exe"
            | "rundll32.exe"
            | "conhost.exe"
            | "rpcss.exe"
            | "svchost.exe"
            | "plugplay.exe"
    ) || name.starts_with("pressure-vessel")
        || name.starts_with("steam-runtime-")
}

fn basename_lc(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

fn executable_arg_path(
    arg: &str,
    cwd: Option<&Path>,
    wine_prefix: Option<&Path>,
) -> Option<PathBuf> {
    if arg.is_empty() || arg.starts_with('-') {
        return None;
    }
    let unix = arg.replace('\\', "/");
    let bytes = unix.as_bytes();
    let path = if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' {
        let drive = bytes[0].to_ascii_lowercase() as char;
        if drive == 'z' {
            PathBuf::from(format!("/{}", &unix[3..]))
        } else {
            wine_prefix?
                .join("dosdevices")
                .join(format!("{drive}:"))
                .join(&unix[3..])
        }
    } else {
        let path = PathBuf::from(arg);
        if path.is_absolute() {
            path
        } else {
            cwd?.join(path)
        }
    };
    Some(canonical_or_original(&path))
}

fn read_processes() -> Result<Vec<ObservedProcess>, String> {
    let uid = rustix::process::getuid().as_raw();
    let mut processes = Vec::new();
    for process in procfs::process::all_processes()
        .map_err(|error| format!("process scan: {error}"))?
        .flatten()
    {
        if process.uid().ok() != Some(uid) {
            continue;
        }
        let Ok(exe) = process.exe() else {
            continue;
        };
        let name = basename_lc(&exe);
        let argv = process.cmdline().unwrap_or_default();
        let cwd = process.cwd().ok();
        let (steam_app_id, wine_prefix) = if name.starts_with("wine") || name.ends_with(".exe") {
            let env = process.environ().unwrap_or_default();
            let app_id = ["SteamAppId", "SteamGameId"].iter().find_map(|key| {
                env.get(std::ffi::OsStr::new(key))
                    .and_then(|value| value.to_str())
                    .filter(|id| is_app_id(id))
                    .map(str::to_owned)
            });
            let prefix = env
                .get(std::ffi::OsStr::new("WINEPREFIX"))
                .map(PathBuf::from);
            (app_id, prefix)
        } else {
            (None, None)
        };
        processes.push(ObservedProcess::from_parts(
            process.pid,
            &exe,
            cwd.as_deref(),
            &argv,
            steam_app_id,
            wine_prefix.as_deref(),
        ));
    }
    Ok(processes)
}

fn is_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.bytes().all(|c| c.is_ascii_digit())
        && id.parse::<u64>().is_ok_and(|id| id > 0)
}

fn steam_window_id(app_id: &str) -> Option<String> {
    app_id
        .strip_prefix("steam_app_")
        .filter(|id| is_app_id(id))
        .map(str::to_owned)
}

fn parse_window_app_ids(backend: &str, bytes: &[u8]) -> Result<Vec<String>, String> {
    let mut ids = BTreeSet::new();
    if backend == "niri" {
        for line in String::from_utf8_lossy(bytes).lines() {
            if let Some(app_id) = line.strip_prefix("  App ID: ") {
                let app_id = app_id.trim().trim_matches('"');
                if !app_id.is_empty() {
                    ids.insert(app_id.to_owned());
                }
            }
        }
    } else {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| format!("{backend} windows: {error}"))?;
        let array = value
            .as_array()
            .or_else(|| value.get("outputs").and_then(|value| value.as_array()))
            .ok_or_else(|| format!("{backend} windows: expected array"))?;
        for item in array {
            if backend == "halley" {
                for node in item
                    .get("nodes")
                    .and_then(|nodes| nodes.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(id) = node
                        .get("app_id")
                        .and_then(|id| id.as_str())
                        .filter(|id| !id.is_empty())
                    {
                        ids.insert(id.to_owned());
                    }
                }
            } else {
                if let Some(id) = item
                    .get("class")
                    .and_then(|id| id.as_str())
                    .filter(|id| !id.is_empty())
                {
                    ids.insert(id.to_owned());
                }
            }
        }
    }
    Ok(ids.into_iter().collect())
}

async fn read_window_app_ids() -> Result<Vec<String>, String> {
    let desktops = [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .iter()
    .filter_map(|key| std::env::var(key).ok())
    .collect::<Vec<_>>()
    .join(":")
    .to_lowercase();
    let (backend, program, args): (&str, &str, &[&str]) =
        if desktops.contains("halley") || std::env::var_os("HALLEY_WL_BACKEND").is_some() {
            ("halley", "halleyctl", &["node", "list", "--json"])
        } else if desktops.contains("hyprland")
            || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        {
            ("hyprland", "hyprctl", &["clients", "-j"])
        } else if desktops.contains("niri") || std::env::var_os("NIRI_SOCKET").is_some() {
            ("niri", "niri", &["msg", "windows"])
        } else {
            return Ok(Vec::new());
        };
    let mut command = tokio::process::Command::new(program);
    command.args(args).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(2), command.output())
        .await
        .map_err(|_| format!("{backend} window query timed out"))?
        .map_err(|error| format!("{backend} window query: {error}"))?;
    if !output.status.success() {
        return Err(format!("{backend} window query failed ({})", output.status));
    }
    parse_window_app_ids(backend, &output.stdout)
}

pub async fn run_games(
    tx: mpsc::Sender<ManagerMsg>,
    mut rules_rx: watch::Receiver<GameRules>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut catalogue = Catalogue::default();
    let mut refreshed_at: Option<Instant> = None;
    let mut last: Option<(u64, GamesInfo)> = None;
    let mut last_window_ids = Vec::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *shutdown.borrow() {
            return;
        }
        let force_refresh = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { return; }
                continue;
            }
            changed = rules_rx.changed() => {
                if changed.is_err() { return; }
                true
            }
            _ = ticker.tick() => false,
        };
        let rules = rules_rx.borrow().clone();
        let mut info = GamesInfo {
            monitoring: rules.monitor_games,
            catalogue_entries: catalogue.games().count(),
            ..Default::default()
        };
        if rules.monitor_games {
            if force_refresh
                || refreshed_at.is_none_or(|time| time.elapsed() >= Duration::from_secs(60))
            {
                let previous = catalogue.clone();
                match tokio::task::spawn_blocking(move || previous.refresh()).await {
                    Ok(refreshed) => catalogue = refreshed,
                    Err(error) => catalogue.errors = vec![format!("catalogue refresh: {error}")],
                }
                refreshed_at = Some(Instant::now());
            }
            info.catalogue_entries = catalogue.games().count();
            info.errors = catalogue.errors.clone();
            let (processes, window_ids) = tokio::join!(
                tokio::task::spawn_blocking(read_processes),
                read_window_app_ids()
            );
            match processes {
                Ok(Ok(processes)) => {
                    let ids = match window_ids {
                        Ok(ids) => {
                            last_window_ids = ids.clone();
                            ids
                        }
                        Err(error) => {
                            info.errors.push(error);
                            last_window_ids.clone()
                        }
                    };
                    let steam_ids = ids
                        .iter()
                        .filter_map(|id| steam_window_id(id))
                        .collect::<Vec<_>>();
                    info.running = catalogue.detect(&processes, &steam_ids);
                    add_extra_games(&mut info.running, &rules.extra_games, &processes, &ids);
                }
                error => {
                    info.errors.push(format!("process scan failed: {error:?}"));
                    if let Some((_, previous)) = &last {
                        info.running = previous.running.clone();
                        info.ignored = previous.ignored.clone();
                    }
                }
            }
        }
        info.apply_rules(rules.monitor_games, &rules.blacklist);
        if last.as_ref() != Some(&(rules.epoch, info.clone())) {
            eventline::info!(
                "games: catalogue={}, running={}, ignored={}, monitoring={}",
                info.catalogue_entries,
                info.running
                    .iter()
                    .map(|game| game.title.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                info.ignored.len(),
                info.monitoring
            );
            for error in &info.errors {
                eventline::warn!("games: {error}");
            }
            last = Some((rules.epoch, info.clone()));
            if tx
                .send(ManagerMsg::Event(Event::GamesChanged {
                    info,
                    now_ms: crate::core::utils::now_ms(),
                }))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NativeGame {
        root: PathBuf,
        child: std::process::Child,
    }

    impl NativeGame {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "stasis-extra-game-test-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let exe = root.join("test-game");
            std::fs::copy("/usr/bin/sleep", &exe).unwrap();
            // Parallel tests can fork while another thread holds a copy's
            // writable descriptor. Retry the transient exec-busy condition.
            let mut attempts = 0;
            let child = loop {
                match std::process::Command::new(&exe).arg("30").spawn() {
                    Ok(child) => break child,
                    Err(error) if error.raw_os_error() == Some(26) && attempts < 20 => {
                        attempts += 1;
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("test game spawn: {error}"),
                }
            };
            Self { root, child }
        }
    }

    impl Drop for NativeGame {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn game(id: &str, root: Option<&str>) -> CatalogueGame {
        CatalogueGame {
            identity: DetectedGame {
                id: format!("steam:{id}"),
                title: format!("Test Game {id}"),
                source: "Steam".into(),
                path_game_dir: root.map(str::to_owned),
                pids: Vec::new(),
                extra_rule: None,
            },
            directory: root.map(PathBuf::from),
            steam_app_id: Some(id.into()),
        }
    }

    fn process(exe: &str, cwd: &str, args: &[&str], steam_id: Option<&str>) -> ObservedProcess {
        ObservedProcess::from_parts(
            42,
            Path::new(exe),
            Some(Path::new(cwd)),
            &args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>(),
            steam_id.map(str::to_owned),
            None,
        )
    }

    #[test]
    fn catalogue_is_not_runtime_evidence_and_window_and_process_hits_deduplicate() {
        let catalogue = Catalogue {
            by_launcher: BTreeMap::from([(
                "Steam".into(),
                vec![game("123", Some("/games/My Game"))],
            )]),
            ..Default::default()
        };
        assert!(catalogue.detect(&[], &[]).is_empty());
        let observed = process("/games/My Game/bin/game", "/", &["game"], None);
        let detected = catalogue.detect(&[observed], &["123".into(), "123".into()]);
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].pids, [42]);
        assert_eq!(detected[0].title, "Test Game 123");
        let missing = catalogue.detect(&[], &["999".into()]);
        assert_eq!(missing[0].id, "steam:999");
        assert_eq!(missing[0].title, "Steam app 999");
    }

    #[test]
    fn native_paths_are_specific_and_editors_terminals_and_launchers_do_not_match() {
        let game = game("123", Some("/games/foo"));
        assert!(process("/games/foo/bin/game", "/", &["game"], None).matches(&game));
        assert!(!process("/games/foobar/game", "/", &["game"], None).matches(&game));
        assert!(!process("/usr/bin/bash", "/games/foo", &["bash"], None).matches(&game));
        assert!(
            !process("/usr/bin/code", "/", &["code", "/games/foo/game.exe"], None).matches(&game)
        );
        assert!(
            !process(
                "/games/foo/steamwebhelper",
                "/games/foo",
                &["steamwebhelper"],
                Some("123")
            )
            .matches(&game)
        );
        assert!(
            !process(
                "/usr/bin/python3",
                "/",
                &[
                    "python3",
                    "/usr/share/proton/proton",
                    "run",
                    "/games/foo/game.exe"
                ],
                Some("123")
            )
            .matches(&game)
        );
    }

    #[test]
    fn shared_directories_are_ambiguous_and_specific_installations_take_precedence() {
        let mut catalogue = Catalogue {
            by_launcher: BTreeMap::from([(
                "Steam".into(),
                vec![
                    game("123", Some("/games/shared")),
                    game("456", Some("/games/shared")),
                ],
            )]),
            ..Default::default()
        };
        let observed = process("/games/shared/bin/game", "/", &["game"], None);
        assert!(catalogue.detect(&[observed], &[]).is_empty());
        catalogue
            .by_launcher
            .get_mut("Steam")
            .unwrap()
            .push(game("789", Some("/games/shared/bin")));
        let observed = process("/games/shared/bin/game", "/", &["game"], None);
        assert_eq!(catalogue.detect(&[observed], &[])[0].id, "steam:789");
        let observed = process("/usr/bin/wine", "/", &["wine", "C:\\game.exe"], Some("123"));
        assert_eq!(catalogue.detect(&[observed], &[])[0].id, "steam:123");
    }

    #[test]
    fn wine_requires_a_game_executable_or_specific_steam_id_and_excludes_helpers() {
        let game = game("123", Some("/games/My Game"));
        assert!(
            process(
                "/usr/bin/wine64-preloader",
                "/",
                &["Z:\\games\\My Game\\game.exe"],
                None
            )
            .matches(&game)
        );
        assert!(
            process(
                "/usr/bin/wine",
                "/games/My Game",
                &["wine", "game.exe"],
                None
            )
            .matches(&game)
        );
        assert!(
            process(
                "/usr/bin/wine64-preloader",
                "/",
                &["C:\\Game\\game.exe"],
                Some("123")
            )
            .matches(&game)
        );
        assert!(
            !process(
                "/usr/bin/wine64-preloader",
                "/",
                &["C:\\Other\\game.exe"],
                Some("456")
            )
            .matches(&game)
        );
        assert!(
            !process(
                "/usr/bin/wine64-preloader",
                "/",
                &["C:\\windows\\system32\\services.exe"],
                Some("123")
            )
            .matches(&game)
        );
        assert!(
            !process(
                "/usr/bin/wineserver",
                "/games/My Game",
                &["wineserver"],
                Some("123")
            )
            .matches(&game)
        );
        assert!(!process("/usr/bin/wine", "/games/My Game", &["wine"], Some("123")).matches(&game));
    }

    #[test]
    fn minecraft_needs_instance_specific_arguments_and_scripts_need_a_script_path() {
        let game = game("123", Some("/games/instance"));
        assert!(
            process(
                "/usr/bin/java",
                "/",
                &["java", "--gameDir", "/games/instance/.minecraft"],
                None
            )
            .matches(&game)
        );
        assert!(
            process(
                "/usr/bin/java",
                "/",
                &["java", "-Djava.library.path=/games/instance/natives"],
                None
            )
            .matches(&game)
        );
        assert!(
            !process(
                "/usr/bin/java",
                "/games/instance",
                &["java", "-jar", "/apps/launcher.jar"],
                None
            )
            .matches(&game)
        );
        assert!(
            process(
                "/usr/bin/python3",
                "/games/instance",
                &["python3", "game.py"],
                None
            )
            .matches(&game)
        );
    }

    #[test]
    fn blacklist_matches_titles_ids_paths_and_regexes_and_can_be_cleared() {
        let observed = game("123", Some("/games/Test Game")).identity;
        for literal in ["test game", "steam:123", "steam_app_123", "/games/test"] {
            let mut info = GamesInfo {
                running: vec![observed.clone()],
                ..Default::default()
            };
            info.apply_rules(true, &[Pattern::Literal(literal.into())]);
            assert!(info.running.is_empty(), "{literal}");
            assert_eq!(info.ignored.len(), 1);
            info.apply_rules(true, &[]);
            assert_eq!(info.running.len(), 1);
            assert!(info.ignored.is_empty());
        }
        let mut info = GamesInfo {
            running: vec![observed],
            ..Default::default()
        };
        info.apply_rules(
            true,
            &[Pattern::Regex(regex::Regex::new("^steam:123$").unwrap())],
        );
        assert!(info.running.is_empty());
        info.apply_rules(false, &[]);
        assert!(info.running.is_empty());
        assert!(info.ignored.is_empty());
    }

    #[test]
    fn extra_rules_detect_missed_executables_directories_and_window_ids_and_respect_blacklist() {
        let wine = process(
            "/usr/bin/wine",
            "/",
            &["wine", "C:\\GOG\\Missed Game\\missed-game.exe"],
            None,
        );
        let rules = [Pattern::Literal("missed-game.exe".into())];
        let mut running = Vec::new();
        add_extra_games(&mut running, &rules, &[wine], &["Missed-Game.exe".into()]);
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].title, "missed-game.exe");
        assert_eq!(running[0].pids, [42]);
        assert_eq!(running[0].extra_rule.as_deref(), Some("missed-game.exe"));
        let mut info = GamesInfo {
            running,
            ..Default::default()
        };
        info.apply_rules(true, &[Pattern::Literal("missed-game".into())]);
        assert!(info.running.is_empty());
        assert_eq!(info.ignored.len(), 1);

        let mut running = Vec::new();
        add_extra_games(
            &mut running,
            &[Pattern::Literal("org.gog.customgame".into())],
            &[],
            &["org.gog.CustomGame".into()],
        );
        assert_eq!(running[0].id, "extra:org.gog.customgame");

        let mut running = Vec::new();
        let native = process("/games/gog/game/bin/start", "/", &["start"], None);
        add_extra_games(
            &mut running,
            &[Pattern::Literal("/games/gog/game".into())],
            &[native],
            &[],
        );
        assert_eq!(running[0].path_game_dir.as_deref(), Some("/games/gog/game"));
        assert_eq!(running[0].title, "game");
        assert!(
            !process("/games/gog/game2/start", "/", &["start"], None)
                .matches_extra(&Pattern::Literal("/games/gog/game".into()))
        );
        assert!(
            !process(
                "/usr/bin/wineserver",
                "/games/gog/game",
                &["wineserver"],
                None
            )
            .matches_extra(&Pattern::Regex(regex::Regex::new("wine").unwrap()))
        );
    }

    #[test]
    fn extra_rules_do_not_duplicate_automatic_games_and_regexes_match_real_identity() {
        let catalogue = Catalogue {
            by_launcher: BTreeMap::from([("Steam".into(), vec![game("123", Some("/games/test"))])]),
            ..Default::default()
        };
        let observed = process("/games/test/game", "/", &["game"], None);
        let mut running = catalogue.detect(&[observed], &["123".into()]);
        let observed = process("/games/test/game", "/", &["game"], None);
        add_extra_games(
            &mut running,
            &[Pattern::Regex(regex::Regex::new("game|steam_app").unwrap())],
            &[observed],
            &["steam_app_123".into()],
        );
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, "steam:123");
        let mut running = Vec::new();
        let observed = process("/games/unregistered/gog-game", "/", &["gog-game"], None);
        add_extra_games(
            &mut running,
            &[Pattern::Regex(regex::Regex::new("^gog-game$").unwrap())],
            &[observed],
            &[],
        );
        assert_eq!(running[0].title, "gog-game");
    }

    #[tokio::test]
    async fn live_extra_game_service_blacklists_removes_and_disables_a_real_process() {
        let fixture = NativeGame::new();
        let pid = fixture.child.id() as i32;
        let extra = Pattern::Literal(fixture.root.to_string_lossy().to_lowercase());
        let (tx, mut rx) = mpsc::channel(8);
        let (rules_tx, rules_rx) = watch::channel(GameRules {
            epoch: 0,
            monitor_games: true,
            blacklist: Vec::new(),
            extra_games: vec![extra.clone()],
        });
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(run_games(tx, rules_rx, shutdown_rx));
        for phase in 0..4 {
            if phase > 0 {
                rules_tx
                    .send(GameRules {
                        epoch: phase,
                        monitor_games: phase != 3,
                        blacklist: if phase == 1 {
                            vec![Pattern::Literal("extra-game".into())]
                        } else {
                            Vec::new()
                        },
                        extra_games: if phase == 2 {
                            Vec::new()
                        } else {
                            vec![extra.clone()]
                        },
                    })
                    .unwrap();
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let Some(ManagerMsg::Event(Event::GamesChanged { info, .. })) = rx.recv().await
                    else {
                        panic!("expected game status");
                    };
                    let running = info.running.iter().any(|game| game.pids.contains(&pid));
                    let ignored = info.ignored.iter().any(|game| game.pids.contains(&pid));
                    if match phase {
                        0 => running,
                        1 => !running && ignored,
                        2 => !running && !ignored,
                        _ => !info.monitoring && !running && !ignored,
                    } {
                        break;
                    }
                }
            })
            .await
            .unwrap();
        }
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn steam_fallback_accepts_only_actual_app_ids_in_supported_window_formats() {
        for value in [
            "Steam",
            "steam_app_",
            "steam_app_zero",
            "steam_app_0",
            "steam_app_123_suffix",
            "something_steam_app_123",
        ] {
            assert!(steam_window_id(value).is_none());
        }
        let current = br#"[{"nodes":[{"app_id":"steam_app_123"},{"app_id":"Steam"}]}]"#;
        let legacy = br#"{"outputs":[{"nodes":[{"app_id":"steam_app_123"}]}]}"#;
        assert_eq!(
            parse_window_app_ids("halley", current).unwrap(),
            ["Steam", "steam_app_123"]
        );
        assert_eq!(
            parse_window_app_ids("halley", legacy).unwrap(),
            ["steam_app_123"]
        );
        assert_eq!(
            parse_window_app_ids(
                "hyprland",
                br#"[{"class":"steam_app_123"},{"class":"wine"}]"#
            )
            .unwrap(),
            ["steam_app_123", "wine"]
        );
        assert_eq!(
            parse_window_app_ids("niri", b"Window ID 1:\n  App ID: \"steam_app_123\"\n").unwrap(),
            ["steam_app_123"]
        );
        assert!(parse_window_app_ids("halley", b"{}").is_err());
        assert!(parse_window_app_ids("hyprland", b"bad json").is_err());
    }

    #[test]
    fn linux_process_scan_detects_a_real_native_process_and_clears_after_exit() {
        let mut fixture = NativeGame::new();
        let game = game("123", Some(fixture.root.to_str().unwrap()));
        let result = read_processes().unwrap();
        assert!(
            result
                .iter()
                .any(|process| process.pid == fixture.child.id() as i32 && process.matches(&game))
        );
        fixture.child.kill().unwrap();
        fixture.child.wait().unwrap();
        assert!(
            !read_processes()
                .unwrap()
                .iter()
                .any(|process| process.pid == fixture.child.id() as i32 && process.matches(&game))
        );
    }

    #[tokio::test]
    async fn disabled_game_service_emits_no_hold_reconfigures_and_shuts_down() {
        let (tx, mut rx) = mpsc::channel(8);
        let (rules_tx, rules_rx) = watch::channel(GameRules {
            epoch: 0,
            monitor_games: false,
            blacklist: Vec::new(),
            extra_games: Vec::new(),
        });
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(run_games(tx, rules_rx, shutdown_rx));
        for epoch in 0..2 {
            if epoch == 1 {
                rules_tx
                    .send(GameRules {
                        epoch,
                        monitor_games: false,
                        blacklist: vec![Pattern::Literal("test".into())],
                        extra_games: Vec::new(),
                    })
                    .unwrap();
            }
            let message = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            let ManagerMsg::Event(Event::GamesChanged { info, .. }) = message else {
                panic!("expected game status");
            };
            assert!(!info.monitoring);
            assert!(info.running.is_empty());
            assert_eq!(info.catalogue_entries, 0);
        }
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "reads the local installed library and writes a probe report"]
    async fn local_library_detection_probe() {
        if let Some(path) = std::env::var_os("STASIS_PROBE_CONFIG") {
            let loaded = crate::config::load_with_fallbacks(Some(Path::new(&path)), &[]).unwrap();
            println!(
                "Config validated: {}; monitor_games={}; blacklist={}",
                loaded.path.display(),
                loaded.cfg.default.monitor_games,
                loaded.cfg.default.game_blacklist.len()
            );
        }
        let catalogue = Catalogue::default().refresh();
        let processes = read_processes().unwrap();
        let app_ids = read_window_app_ids().await.unwrap();
        let ids = app_ids
            .iter()
            .filter_map(|id| steam_window_id(id))
            .collect::<Vec<_>>();
        let detected = catalogue.detect(&processes, &ids);
        let report = serde_json::json!({
            "catalogue_entries": catalogue.games().count(), "errors": catalogue.errors,
            "catalogue": catalogue.games().map(|game| &game.identity).collect::<Vec<_>>(),
            "running": detected,
        });
        std::fs::create_dir_all("target/game-detector-probe").unwrap();
        std::fs::write(
            "target/game-detector-probe/runtime-probe.json",
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();
        println!(
            "Catalogue: {}; running: {}; errors: {:?}",
            catalogue.games().count(),
            report["running"],
            catalogue.errors
        );
    }
}

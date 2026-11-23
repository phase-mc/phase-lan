//! Mod synchronization between the host and guests.
//!
//! **Host** ([`apply_host_mods`]): finds the `mods` folder, computes hashes (with a cache) and
//! builds the distribution: mods with valid names, except those the host
//! unticked. The distribution can be rebuilt while the room is open.
//!
//! **Guest** ([`GuestSync`]) lives as long as the session:
//!
//! 1. *Check.* Requests the host's mod list, scans its own folder and compares
//!    by BLAKE3: what is missing and what is extra ([`diff`]).
//! 2. *Waiting.* Waits for a command: "sync", "skip", "check again",
//!    "restore my mods". In auto mode the sync starts by itself.
//! 3. *Sync.* Extra mods move to `mods/.phase-backup/` (they are not
//!    deleted). Each missing mod is downloaded in two steps: first from Modrinth (by
//!    SHA-1), and if it is not there or Modrinth is unavailable, directly from the host through
//!    the tunnel. The file is written to `.part`, checked by BLAKE3 and only then becomes
//!    `.jar`.
//!
//! The game tunnel does not depend on sync: the guest can join the world without mods too.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use phase_lang::tr;
use phase_minecraft::mods::{self as mc_mods, BackupInfo, HashCache, ModFile, ModsDir};
use phase_tunnel::{
    Connection,
    mods::{self, ModEntry, ModManifest, ModShare, SharedMod},
    wire::ModsOffer,
};
use tokio::sync::{mpsc, watch};

use crate::{
    modrinth::{self, Lookup},
    options::{GuestMods, HostMods, ModsFolder},
    session::{Control, Ctx, ModCommand},
    status::{HostModsStatus, ModProgress, ModSource, ModSyncState, ModSyncStatus},
};

/// How many names of missing and extra mods to show in the interface.
const NAMES_SHOWN: usize = 200;
/// How often to update download progress (each update is an event for the window).
const PROGRESS_EVERY: Duration = Duration::from_millis(200);
/// How long to wait for the connection to the host to come back.
const HOST_WAIT: Duration = Duration::from_secs(60);
/// How many times to resume a mod download after a connection drop.
const P2P_ATTEMPTS: u32 = 5;

/// Size for messages, e.g. "12.3 MB" (the unit follows the app language).
fn size_text(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 0.1 {
        tr!(
            format!("{mb:.1} МБ").replace('.', ","),
            format!("{mb:.1} MB")
        )
    } else {
        tr!(
            format!("{} КБ", bytes.div_ceil(1024)),
            format!("{} KB", bytes.div_ceil(1024))
        )
    }
}

/// Mods of the folder with hashes; the hash cache is in a file (if one is set).
fn scan_dir(dir: &Path, cache_file: &Option<PathBuf>) -> Result<Vec<ModFile>> {
    let mut cache = match cache_file {
        Some(file) => HashCache::load(file),
        None => HashCache::in_memory(),
    };
    let found = mc_mods::scan(dir, &mut cache).with_context(|| {
        tr!(
            format!("не удалось прочитать папку модов {}", dir.display()),
            format!("could not read the mods folder {}", dir.display())
        )
    })?;
    if let Err(err) = cache.save() {
        tracing::warn!("кэш хэшей модов не сохранён: {err:#}");
    }
    Ok(found)
}

fn locate(folder: &ModsFolder) -> Result<ModsDir> {
    mc_mods::locate(folder.chosen.as_deref(), folder.detect_running_game).context(tr!(
        "папка mods не найдена: выберите её на вкладке «Моды»",
        "the mods folder was not found: choose it on the Mods tab"
    ))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work).await.map_err(|err| {
        anyhow!(
            "{}: {err}",
            tr!(
                "работа с папкой модов аварийно завершилась",
                "working with the mods folder crashed"
            )
        )
    })?
}

// ------------------------------------------------------------------ host

/// Rebuilds the host's distribution from the settings and describes it for the interface (the
/// sent counters are zero: the session keeps them).
pub(crate) async fn apply_host_mods(
    share: &ModShare,
    wanted: &HostMods,
    cache_file: &Option<PathBuf>,
) -> HostModsStatus {
    if !wanted.share {
        share.clear();
        return HostModsStatus::default();
    }
    let mut status = HostModsStatus {
        sharing: true,
        ..HostModsStatus::default()
    };
    let (folder, cache_file) = (wanted.folder.clone(), cache_file.clone());
    let scanned = blocking(move || {
        let dir = locate(&folder)?;
        let found = scan_dir(&dir.path, &cache_file)?;
        Ok((dir, found))
    })
    .await;
    let (dir, found) = match scanned {
        Ok(scanned) => scanned,
        Err(err) => {
            share.clear();
            status.error = Some(format!("{err:#}"));
            return status;
        }
    };
    status.dir = Some(dir.path.display().to_string());
    status.dir_source = Some(dir.source);

    let excluded: HashSet<String> = wanted.excluded.iter().map(|n| n.to_lowercase()).collect();
    let (mut shared, mut optional) = (Vec::new(), Vec::new());
    for file in found {
        if mods::validate_file_name(&file.file_name).is_err() {
            // The guest will reject such a name anyway.
            status.skipped.push(file.file_name);
        } else if mc_mods::is_companion_jar(&file.file_name)
            || excluded.contains(&file.file_name.to_lowercase())
        {
            status.excluded += 1;
            optional.push(file.blake3);
        } else {
            shared.push(SharedMod {
                path: dir.path.join(&file.file_name),
                entry: ModEntry {
                    file_name: file.file_name,
                    size: file.size,
                    sha1: file.sha1,
                    blake3: file.blake3,
                },
            });
        }
    }
    match share.set(shared, optional) {
        Ok(()) => {
            let offer = share.offer();
            status.files = offer.files;
            status.total_size = offer.total_size;
        }
        Err(err) => {
            share.clear();
            status.error = Some(format!("{err:#}"));
        }
    }
    status
}

// ------------------------------------------------------------------ check

/// Compares the host's mods with the guest's folder by BLAKE3. Returns the missing mods and the names
/// of the guest's extra files.
///
/// - A mod with the same contents under a different name does not count as missing.
/// - A guest mod that the host has but does not distribute ([`ModManifest::optional`]) is not
///   extra.
/// - A guest file whose name is taken by a missing host mod (another version of the same mod)
///   is always extra: otherwise the new file would be written over it.
pub fn diff(manifest: &ModManifest, local: &[ModFile]) -> (Vec<ModEntry>, Vec<String>) {
    let known: HashSet<&str> = manifest
        .mods
        .iter()
        .map(|m| m.blake3.as_str())
        .chain(manifest.optional.iter().map(String::as_str))
        .collect();
    let mut extra: HashSet<usize> = (0..local.len())
        .filter(|&i| !known.contains(local[i].blake3.as_str()))
        .collect();
    loop {
        let kept: HashSet<&str> = (0..local.len())
            .filter(|i| !extra.contains(i))
            .map(|i| local[i].blake3.as_str())
            .collect();
        let incoming: HashSet<String> = manifest
            .mods
            .iter()
            .filter(|m| !kept.contains(m.blake3.as_str()))
            .map(|m| m.file_name.to_lowercase())
            .collect();
        let before = extra.len();
        extra.extend(
            (0..local.len()).filter(|&i| incoming.contains(&local[i].file_name.to_lowercase())),
        );
        if extra.len() == before {
            let missing = manifest
                .mods
                .iter()
                .filter(|m| !kept.contains(m.blake3.as_str()))
                .cloned()
                .collect();
            let extra = (0..local.len())
                .filter(|i| extra.contains(i))
                .map(|i| local[i].file_name.clone())
                .collect();
            return (missing, extra);
        }
    }
}

/// Result of the check.
struct Plan {
    dir: ModsDir,
    manifest: ModManifest,
    missing: Vec<ModEntry>,
    extra: Vec<String>,
    backup: BackupInfo,
}

impl Plan {
    fn needs_work(&self) -> bool {
        !self.missing.is_empty() || !self.extra.is_empty()
    }

    fn state(&self) -> ModSyncState {
        if self.manifest.mods.is_empty() {
            ModSyncState::NotRequired
        } else if self.needs_work() {
            ModSyncState::NeedsSync
        } else {
            ModSyncState::Done
        }
    }
}

/// Result of the sync.
#[derive(Default)]
struct SyncReport {
    from_modrinth: u32,
    from_host: u32,
    backed_up: u32,
    /// Extra mods that could not be removed (busy with the game).
    stuck: Vec<String>,
    changed: bool,
}

// ------------------------------------------------------------------ guest

/// The guest's mod sync: lives while the session runs.
pub(crate) struct GuestSync {
    pub ctx: Ctx,
    pub opts: GuestMods,
    pub cache_file: Option<PathBuf>,
    /// Connection to the host (`None` while reconnecting).
    pub conn: watch::Receiver<Option<Connection>>,
    /// What the host announced in the greeting (`None` means the host has an old version).
    pub offer: Option<ModsOffer>,
}

impl GuestSync {
    /// Initial state for [`crate::GuestReady`] while the check has not run yet.
    pub fn initial_status(&self) -> ModSyncStatus {
        let state = if self.offer.is_some_and(|offer| offer.files > 0) {
            ModSyncState::Checking
        } else {
            ModSyncState::NotRequired
        };
        ModSyncStatus::new(state, self.offer.is_some(), self.opts.auto)
    }

    pub async fn run(self, mut commands: mpsc::UnboundedReceiver<Control>) {
        let plan = self.check().await;
        if self.opts.auto
            && let Some(plan) = plan.filter(Plan::needs_work)
        {
            self.sync(plan, &mut commands).await;
        }
        while let Some(control) = commands.recv().await {
            let Control::Mods(command) = control else {
                continue;
            };
            match command {
                ModCommand::Recheck => {
                    self.check().await;
                }
                ModCommand::Sync => {
                    // Check again: the folder could have changed while the guest was thinking.
                    if let Some(plan) = self.check().await.filter(Plan::needs_work) {
                        self.sync(plan, &mut commands).await;
                    }
                }
                ModCommand::Skip => self.skip(),
                ModCommand::Restore => self.restore().await,
            }
        }
    }

    fn set(&self, change: impl FnOnce(&mut ModSyncStatus)) {
        self.ctx.update(|status| {
            if let Some(guest) = &mut status.guest {
                change(&mut guest.mods);
            }
        });
    }

    fn fail(&self, what: &str, err: &anyhow::Error) {
        let text = format!("{err:#}");
        self.ctx.warn(format!("{what}: {text}"));
        self.set(|m| {
            m.state = ModSyncState::Failed;
            m.progress = None;
            m.error = Some(text);
        });
    }

    fn skip(&self) {
        self.ctx.info(tr!(
            "Вход без модов хоста: синхронизация пропущена.",
            "Joining without the host's mods: Mod Sync skipped."
        ));
        self.set(|m| {
            m.state = ModSyncState::Skipped;
            m.progress = None;
            m.error = None;
        });
    }

    /// The current connection to the host; on a drop it waits for a reconnect.
    async fn connection(&self) -> Result<Connection> {
        let mut conn = self.conn.clone();
        let found = tokio::time::timeout(HOST_WAIT, conn.wait_for(Option::is_some))
            .await
            .context(tr!("нет связи с хостом", "no connection to the host"))?
            .map_err(|_| anyhow!(tr!("сессия завершается", "the session is ending")))?
            .clone();
        found.context(tr!("нет связи с хостом", "no connection to the host"))
    }

    /// Check: the host's list against your own folder. The result is in the session state.
    async fn check(&self) -> Option<Plan> {
        if self.offer.is_some() {
            self.set(|m| {
                m.state = ModSyncState::Checking;
                m.progress = None;
                m.error = None;
            });
        }
        match self.try_check().await {
            Ok(plan) => {
                let state = plan.state();
                self.publish(&plan, state);
                match state {
                    ModSyncState::NeedsSync => self.ctx.info(tr!(
                        format!(
                            "Моды: у хоста {}, вам не хватает {} ({}), лишних у вас {}.",
                            plan.manifest.mods.len(),
                            plan.missing.len(),
                            size_text(plan.missing.iter().map(|m| m.size).sum()),
                            plan.extra.len()
                        ),
                        format!(
                            "Mods: the host has {}, you are missing {} ({}), and you have {} extra.",
                            plan.manifest.mods.len(),
                            plan.missing.len(),
                            size_text(plan.missing.iter().map(|m| m.size).sum()),
                            plan.extra.len()
                        )
                    )),
                    ModSyncState::Done => self.ctx.info(tr!(
                        format!(
                            "Моды совпадают с модами хоста ({}).",
                            plan.manifest.mods.len()
                        ),
                        format!("Your mods match the host's ({}).", plan.manifest.mods.len())
                    )),
                    _ => {}
                }
                Some(plan)
            }
            Err(err) => {
                self.fail(tr!("Сверка модов не удалась", "Mod check failed"), &err);
                None
            }
        }
    }

    async fn try_check(&self) -> Result<Plan> {
        // A host of an old version does not know the mod stream: we do not request its list.
        let manifest = match self.offer {
            Some(_) => mods::fetch_manifest(&self.connection().await?).await?,
            None => ModManifest::default(),
        };
        let (folder, cache_file) = (self.opts.folder.clone(), self.cache_file.clone());
        let required = !manifest.mods.is_empty();
        let (dir, local, backup) = blocking(move || {
            let dir = locate(&folder)?;
            // With no mods at the host there is no need to read your own folder.
            let local = if required {
                scan_dir(&dir.path, &cache_file)?
            } else {
                Vec::new()
            };
            let backup = mc_mods::backup_info(&dir.path);
            Ok((dir, local, backup))
        })
        .await?;
        let (missing, extra) = if required {
            diff(&manifest, &local)
        } else {
            (Vec::new(), Vec::new())
        };
        Ok(Plan {
            dir,
            manifest,
            missing,
            extra,
            backup,
        })
    }

    fn publish(&self, plan: &Plan, state: ModSyncState) {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        self.set(|m| {
            m.state = state;
            m.dir = Some(plan.dir.path.display().to_string());
            m.dir_source = Some(plan.dir.source);
            m.required = count(plan.manifest.mods.len());
            m.required_bytes = plan.manifest.total_size();
            m.missing = count(plan.missing.len());
            m.missing_bytes = plan.missing.iter().map(|e| e.size).sum();
            m.missing_names = plan
                .missing
                .iter()
                .take(NAMES_SHOWN)
                .map(|e| e.file_name.clone())
                .collect();
            m.extra = count(plan.extra.len());
            m.extra_names = plan.extra.iter().take(NAMES_SHOWN).cloned().collect();
            m.backup_files = count(plan.backup.files);
            m.can_restore = plan.backup.can_restore();
            m.progress = None;
        });
    }

    /// Sync following the check. "Skip" during a download interrupts it
    /// (a partly downloaded file stays in `.part` and will be resumed next time).
    async fn sync(&self, plan: Plan, commands: &mut mpsc::UnboundedReceiver<Control>) {
        self.ctx.info(tr!(
            format!(
                "Синхронизирую моды: скачать {} ({}), убрать в бэкап {}.",
                plan.missing.len(),
                size_text(plan.missing.iter().map(|m| m.size).sum()),
                plan.extra.len()
            ),
            format!(
                "Syncing mods: downloading {} ({}), moving {} to backup.",
                plan.missing.len(),
                size_text(plan.missing.iter().map(|m| m.size).sum()),
                plan.extra.len()
            )
        ));
        self.set(|m| {
            m.state = ModSyncState::Syncing;
            m.error = None;
            m.from_modrinth = 0;
            m.from_host = 0;
            m.backed_up = 0;
            m.needs_mc_restart = false;
        });
        let outcome = {
            let work = self.run_sync(&plan);
            tokio::pin!(work);
            loop {
                tokio::select! {
                    result = &mut work => break Some(result),
                    control = commands.recv() => match control {
                        Some(Control::Mods(ModCommand::Skip)) | None => break None,
                        // Other commands make no sense during a download.
                        Some(_) => {}
                    },
                }
            }
        };
        let report = match outcome {
            None => return self.skip(),
            Some(Err(err)) => {
                return self.fail(
                    tr!("Синхронизация модов не удалась", "Mod Sync failed"),
                    &err,
                );
            }
            Some(Ok(report)) => report,
        };

        // The game is already running: it will see the new mods only after a restart.
        let game_running = report.changed
            && self.opts.folder.detect_running_game
            && blocking(|| Ok(mc_mods::is_game_running()))
                .await
                .unwrap_or(false);
        // The result is by a new check: what is on disk in fact.
        let after = match self.try_check().await {
            Ok(after) => after,
            Err(err) => {
                return self.fail(
                    tr!(
                        "Сверка модов после синхронизации не удалась",
                        "Checking mods after Mod Sync failed"
                    ),
                    &err,
                );
            }
        };
        let state = after.state();
        self.publish(&after, state);
        let error = (!report.stuck.is_empty()).then(|| {
            tr!(
                format!(
                    "Не удалось убрать {} мод(ов): файлы заняты. Закройте Minecraft и \
                     синхронизируйте ещё раз. Первый: {}",
                    report.stuck.len(),
                    report.stuck[0]
                ),
                format!(
                    "Could not move {} mod(s) out of the way: the files are in use. Close Minecraft and \
                     sync again. First one: {}",
                    report.stuck.len(),
                    report.stuck[0]
                )
            )
        });
        if let Some(error) = &error {
            self.ctx.warn(error.clone());
        }
        self.set(|m| {
            m.from_modrinth = report.from_modrinth;
            m.from_host = report.from_host;
            m.backed_up = report.backed_up;
            m.needs_mc_restart = game_running;
            m.error = error;
        });
        self.ctx.info(tr!(
            format!(
                "Моды установлены: {} с Modrinth, {} у хоста; ваших модов в бэкапе — {}.",
                report.from_modrinth, report.from_host, after.backup.files
            ),
            format!(
                "Mods installed: {} from Modrinth, {} from the host; your own mods in backup: {}.",
                report.from_modrinth, report.from_host, after.backup.files
            )
        ));
        if game_running {
            self.ctx.info(tr!(
                "Моды установлены! Перезапустите Minecraft, чтобы игра загрузила их.",
                "Mods installed! Restart Minecraft so the game loads them."
            ));
        }
    }

    async fn run_sync(&self, plan: &Plan) -> Result<SyncReport> {
        let dir = plan.dir.path.clone();
        let mut report = SyncReport::default();

        // 1. Extra mods go to the backup (they are not deleted).
        let stash = {
            let (dir, extra) = (dir.clone(), plan.extra.clone());
            blocking(move || {
                std::fs::create_dir_all(&dir).with_context(|| {
                    tr!(
                        format!("не удалось создать папку {}", dir.display()),
                        format!("could not create the folder {}", dir.display())
                    )
                })?;
                mc_mods::stash(&dir, &extra)
            })
            .await?
        };
        for (name, why) in &stash.failed {
            tracing::warn!("мод {name} не убран в бэкап: {why}");
        }
        report.backed_up = u32::try_from(stash.moved.len()).unwrap_or(u32::MAX);
        report.changed = !stash.moved.is_empty() || !stash.removed.is_empty();
        report.stuck = stash.failed.into_iter().map(|(name, _)| name).collect();
        self.set(|m| m.backed_up = report.backed_up);

        // 2. Missing mods: Modrinth, then the host.
        let modrinth = self.opts.modrinth.clone().and_then(|source| {
            modrinth::Client::new(source)
                .inspect_err(|err| tracing::warn!("Modrinth не используется: {err:#}"))
                .ok()
        });
        let mut modrinth_works = modrinth.is_some();
        let files_total = u32::try_from(plan.missing.len()).unwrap_or(u32::MAX);
        let bytes_total: u64 = plan.missing.iter().map(|e| e.size).sum();
        let mut bytes_before = 0u64;
        for (index, entry) in plan.missing.iter().enumerate() {
            let part = mc_mods::part_path(&dir, &entry.file_name)?;
            let progress = |source: ModSource, file_bytes: u64| ModProgress {
                file: entry.file_name.clone(),
                file_index: u32::try_from(index + 1).unwrap_or(u32::MAX),
                files_total,
                bytes_done: bytes_before + file_bytes.min(entry.size),
                bytes_total,
                source,
            };
            // Download progress goes into the state no more often than once per PROGRESS_EVERY.
            let mut last = Instant::now();
            let mut report_bytes = |source: ModSource, file_bytes: u64| {
                if last.elapsed() >= PROGRESS_EVERY {
                    last = Instant::now();
                    self.set(|m| m.progress = Some(progress(source, file_bytes)));
                }
            };

            let mut source = ModSource::P2p;
            if modrinth_works && let Some(client) = &modrinth {
                match client.lookup(entry).await {
                    Lookup::Found(url) => {
                        self.set(|m| m.progress = Some(progress(ModSource::Modrinth, 0)));
                        let done = client
                            .download(&url, entry, &part, |n| report_bytes(ModSource::Modrinth, n))
                            .await;
                        match done {
                            Ok(()) => source = ModSource::Modrinth,
                            Err(err) => {
                                tracing::info!(
                                    "мод {} с Modrinth не скачан ({err:#}) — беру у хоста",
                                    entry.file_name
                                );
                                let _ = tokio::fs::remove_file(&part).await;
                            }
                        }
                    }
                    Lookup::NotFound => {}
                    Lookup::Unavailable(why) => {
                        modrinth_works = false;
                        self.ctx.info(tr!(
                            format!(
                                "Modrinth недоступен ({why}) — моды скачиваются напрямую у хоста."
                            ),
                            format!("Modrinth is unavailable ({why}) — mods are downloaded directly from the host.")
                        ));
                    }
                }
            }
            if source == ModSource::P2p {
                self.set(|m| m.progress = Some(progress(ModSource::P2p, 0)));
                self.fetch_from_host(entry, &part, |n| report_bytes(ModSource::P2p, n))
                    .await?;
            }

            // 3. The verified file takes its place.
            let (dir, name) = (dir.clone(), entry.file_name.clone());
            blocking(move || mc_mods::install(&dir, &name).map(|_| ())).await?;
            tracing::info!(
                "мод {} установлен ({})",
                entry.file_name,
                match source {
                    ModSource::Modrinth => "Modrinth",
                    ModSource::P2p => "у хоста",
                }
            );
            match source {
                ModSource::Modrinth => report.from_modrinth += 1,
                ModSource::P2p => report.from_host += 1,
            }
            report.changed = true;
            self.set(|m| {
                m.from_modrinth = report.from_modrinth;
                m.from_host = report.from_host;
                m.progress = Some(progress(source, entry.size));
            });
            bytes_before += entry.size;
        }
        Ok(report)
    }

    /// Downloads a mod from the host. After a connection drop it waits for a reconnect and resumes from
    /// the same place.
    async fn fetch_from_host(
        &self,
        entry: &ModEntry,
        part: &Path,
        mut progress: impl FnMut(u64) + Send,
    ) -> Result<()> {
        let mut attempt = 1;
        loop {
            let conn = self.connection().await?;
            let Err(err) = mods::fetch_file(&conn, entry, part, &mut progress).await else {
                return Ok(());
            };
            // An error on a live connection (host refusal, hash mismatch) is final.
            if conn.close_reason().is_none() || attempt >= P2P_ATTEMPTS {
                return Err(err);
            }
            tracing::info!(
                "загрузка мода {} прервана обрывом связи — продолжу после переподключения",
                entry.file_name
            );
            attempt += 1;
            // The connection is closed: we wait until the guest notices the drop and replaces it.
            let mut changed = self.conn.clone();
            let _ = tokio::time::timeout(HOST_WAIT, changed.changed()).await;
        }
    }

    /// "Restore my mods": cancels the sync (see [`mc_mods::restore`]).
    async fn restore(&self) {
        let folder = self.opts.folder.clone();
        let detect = folder.detect_running_game;
        let restored = blocking(move || {
            let dir = locate(&folder)?;
            let report = mc_mods::restore(&dir.path)?;
            let changed = !report.restored.is_empty() || !report.removed.is_empty();
            Ok((report, changed && detect && mc_mods::is_game_running()))
        })
        .await;
        let (report, game_running) = match restored {
            Ok(restored) => restored,
            Err(err) => {
                return self.fail(
                    tr!(
                        "Не удалось вернуть моды из бэкапа",
                        "Could not restore your mods from backup"
                    ),
                    &err,
                );
            }
        };
        self.ctx.info(tr!(
            format!(
                "Ваши моды возвращены из бэкапа: {}. Убрано модов хоста: {}.",
                report.restored.len(),
                report.removed.len()
            ),
            format!(
                "Your mods were restored from backup: {}. Host mods removed: {}.",
                report.restored.len(),
                report.removed.len()
            )
        ));
        let mut problems = Vec::new();
        if !report.conflicts.is_empty() {
            problems.push(tr!(
                format!(
                    "{} мод(ов) остались в бэкапе: в папке уже есть файлы с такими именами",
                    report.conflicts.len()
                ),
                format!(
                    "{} mod(s) stayed in backup: the folder already has files with the same names",
                    report.conflicts.len()
                )
            ));
        }
        if !report.failed.is_empty() {
            problems.push(tr!(
                format!(
                    "{} файл(ов) заняты — закройте Minecraft и повторите",
                    report.failed.len()
                ),
                format!(
                    "{} file(s) are in use — close Minecraft and try again",
                    report.failed.len()
                )
            ));
        }
        let error = (!problems.is_empty()).then(|| problems.join("; "));
        if let Some(error) = &error {
            self.ctx.warn(tr!(
                format!("Возврат модов: {error}."),
                format!("Restoring mods: {error}.")
            ));
        }
        // Counters follow a new check; the sync itself does not start again.
        match self.try_check().await {
            Ok(plan) => {
                let state = match plan.state() {
                    ModSyncState::NeedsSync => ModSyncState::Skipped,
                    other => other,
                };
                self.publish(&plan, state);
                self.set(|m| {
                    m.needs_mc_restart = game_running;
                    m.error = error;
                });
            }
            Err(err) => self.fail(tr!("Сверка модов не удалась", "Mod check failed"), &err),
        }
    }
}

/// "Restore my mods" without a session: the guest has already disconnected, and the host's mods are still in the folder.
/// Returns how many mods came back and how many host mods were removed. The call blocks.
pub fn restore_backup(folder: &ModsFolder) -> Result<mc_mods::RestoreReport> {
    mc_mods::restore(&locate(folder)?.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(name: &str, content: &str) -> ModFile {
        let hashes = mc_mods::hash_bytes(content.as_bytes());
        ModFile {
            file_name: name.into(),
            size: content.len() as u64,
            sha1: hashes.sha1,
            blake3: hashes.blake3,
        }
    }

    fn remote(name: &str, content: &str) -> ModEntry {
        let file = local(name, content);
        ModEntry {
            file_name: file.file_name,
            size: file.size,
            sha1: file.sha1,
            blake3: file.blake3,
        }
    }

    fn names(entries: &[ModEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.file_name.as_str()).collect()
    }

    #[test]
    fn diff_compares_by_content_not_by_name() {
        let manifest = ModManifest {
            mods: vec![
                remote("sodium.jar", "sodium 0.5"),
                remote("lithium.jar", "lithium"),
                remote("fabric-api.jar", "api"),
            ],
            // The host has shaders but does not distribute them.
            optional: vec![local("iris.jar", "iris").blake3],
        };
        let mine = [
            // The same contents under a different name: not downloaded again.
            local("lithium (1).jar", "lithium"),
            // Another version of the host's mod: extra, the host's version will take its place.
            local("sodium.jar", "sodium 0.4"),
            // The guest's own shaders, the same as the host's, stay.
            local("my-iris.jar", "iris"),
            // A mod the host does not have at all.
            local("minimap.jar", "minimap"),
        ];
        let (missing, extra) = diff(&manifest, &mine);
        assert_eq!(names(&missing), ["sodium.jar", "fabric-api.jar"]);
        assert_eq!(extra, ["sodium.jar", "minimap.jar"]);

        // Everything matches: nothing to do.
        let same = [
            local("a.jar", "sodium 0.5"),
            local("b.jar", "lithium"),
            local("c.jar", "api"),
        ];
        assert_eq!(diff(&manifest, &same), (Vec::new(), Vec::new()));
        // The guest's folder is empty: everything is needed.
        assert_eq!(diff(&manifest, &[]).0.len(), 3);
        // The host distributes nothing: we touch nothing at the guest.
        assert_eq!(diff(&ModManifest::default(), &mine).0, Vec::new());
    }

    #[test]
    fn diff_never_lets_a_download_overwrite_a_needed_file() {
        // The guest has the needed mod under a name that is taken by another host mod.
        let manifest = ModManifest {
            mods: vec![remote("a.jar", "new a"), remote("b.jar", "old a")],
            optional: Vec::new(),
        };
        let mine = [local("a.jar", "old a")];
        let (missing, extra) = diff(&manifest, &mine);
        // The file goes to the backup, both mods are downloaded under their own names.
        assert_eq!(extra, ["a.jar"]);
        assert_eq!(names(&missing), ["a.jar", "b.jar"]);
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(size_text(0), "0 КБ");
        assert_eq!(size_text(1500), "2 КБ");
        assert_eq!(size_text(12 * 1024 * 1024 + 300 * 1024), "12,3 МБ");
    }

    #[test]
    fn both_file_name_checks_agree() {
        // A name is checked twice: when the list is received (tunnel) and before writing to disk
        // (minecraft). The rules must match: otherwise one layer would let through what
        // the other rejects, and sync would stop halfway.
        for name in [
            "sodium.jar",
            "Fabric API 0.92.2+1.20.1.jar",
            "мод [1.20].JAR",
            "a.b.c.jar",
            "",
            ".jar",
            "../evil.jar",
            "..\\evil.jar",
            "a/b.jar",
            "C:\\evil.jar",
            "C:evil.jar",
            "evil..jar",
            ".hidden.jar",
            "evil\0.jar",
            "evil\n.jar",
            " evil.jar",
            "evil.jar ",
            "evil.jar.exe",
            "evil.jar.",
            "what?.jar",
            "a*b.jar",
            "CON.jar",
            "com1.x.jar",
            &format!("{}.jar", "x".repeat(196)),
            &format!("{}.jar", "x".repeat(197)),
        ] {
            assert_eq!(
                mods::validate_file_name(name).is_ok(),
                mc_mods::validate_file_name(name).is_ok(),
                "{name:?}"
            );
        }
    }
}

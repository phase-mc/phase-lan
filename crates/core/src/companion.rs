//! Auto-install of Phase Companion: Phase itself puts the server mod into the host's `mods`.
//!
//! Where from: the `companion.json` manifest in the latest GitHub release of the mod's repository.
//! What is in the manifest: an entry for each "loader + game version" pair: file name,
//! address, size and BLAKE3. The downloaded file is installed only if the size and hash match the
//! manifest and the address is on an allowed host (GitHub). No data about the host
//! goes out in the requests: they are two ordinary GETs without parameters.
//!
//! The game picks up mods at launch, so after installing, with the game running,
//! the host needs to restart it once ([`Installed::restart`]).

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use phase_lang::tr;
use phase_minecraft::mods as mc_mods;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

/// Where the manifest comes from by default.
pub const MANIFEST_URL: &str =
    "https://github.com/phase-mc/phase-companion/releases/latest/download/companion.json";
/// Where the mod files may be downloaded from: GitHub releases serve them from these addresses.
const ALLOWED_PREFIXES: &[&str] = &[
    "https://github.com/phase-mc/phase-companion/releases/",
    "https://objects.githubusercontent.com/",
    "https://release-assets.githubusercontent.com/",
];
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_MANIFEST: usize = 256 * 1024;
/// The mod is tens of kilobytes; anything larger is not it.
const MAX_JAR: u64 = 4 * 1024 * 1024;
const SCHEMA: u32 = 1;

/// The host's mod loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Loader {
    Fabric,
    Forge,
    Neoforge,
}

/// Determines the loader of the host's game. The call blocks. `None` means it could not be determined, and nothing
/// is installed. In order, from the most reliable:
///
/// 1. a running game with this folder: by its command line ([`loader_from_command_line`]);
/// 2. mod metadata in the folder: `fabric.mod.json`, `META-INF/mods.toml`,
///    `META-INF/neoforge.mods.toml` ([`loader_from_mods`]);
/// 3. a `.fabric` folder next to `mods` (Fabric without mods).
///
/// The `.fabric` folder goes last: with TLauncher the game folder is shared by all versions, and after
/// a Fabric launch it stays even when the host plays on Forge.
pub fn detect_loader(mods_dir: &Path) -> Option<Loader> {
    let running = mc_mods::running_game_command_line(mods_dir);
    let metadata: Vec<mc_mods::JarMetadata> = mc_mods::list_jars(mods_dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|jar| !mc_mods::is_companion_jar(&jar.file_name))
        .filter_map(|jar| mc_mods::jar_metadata(&mods_dir.join(&jar.file_name)).ok())
        .collect();
    let dot_fabric = mods_dir
        .parent()
        .is_some_and(|game| game.join(".fabric").is_dir());
    pick_loader(running.as_deref(), &metadata, dot_fabric)
}

fn pick_loader(
    running: Option<&str>,
    metadata: &[mc_mods::JarMetadata],
    dot_fabric: bool,
) -> Option<Loader> {
    running
        .and_then(loader_from_command_line)
        .or_else(|| loader_from_mods(metadata))
        .or(dot_fabric.then_some(Loader::Fabric))
}

/// Loader by the game's command line. Forge and NeoForge pass the game their version
/// (`--fml.forgeVersion 52.1.21`, `--fml.neoForgeVersion 21.1.266`), Fabric starts the game
/// with its own class `net.fabricmc.loader…KnotClient`. A fallback sign is the version name in
/// the launcher (`--version 1.21.1-forge-52.1.21`, `--version "Forge 1.21.1"`).
pub fn loader_from_command_line(command_line: &str) -> Option<Loader> {
    // Library paths (`net\neoforged\…`) and class names (`net.neoforged…`) in one form.
    let lower = command_line.to_ascii_lowercase().replace(['\\', '/'], ".");
    if lower.contains("--fml.neoforgeversion") || lower.contains("net.neoforged.") {
        return Some(Loader::Neoforge);
    }
    if lower.contains("--fml.forgeversion") || lower.contains("net.minecraftforge.") {
        return Some(Loader::Forge);
    }
    if lower.contains("net.fabricmc.") {
        return Some(Loader::Fabric);
    }
    let version = mc_mods::flag_value(command_line, "--version")?.to_ascii_lowercase();
    if version.contains("neoforge") {
        Some(Loader::Neoforge)
    } else if version.contains("forge") {
        Some(Loader::Forge)
    } else if version.contains("fabric") {
        Some(Loader::Fabric)
    } else {
        None
    }
}

/// Loader by mod metadata: each mod with metadata of exactly one loader
/// votes for it; a mod for several loaders at once does not vote. A tie gives `None`.
pub fn loader_from_mods(metadata: &[mc_mods::JarMetadata]) -> Option<Loader> {
    let mut votes = [
        (Loader::Fabric, 0usize),
        (Loader::Forge, 0),
        (Loader::Neoforge, 0),
    ];
    for meta in metadata {
        let kinds = [meta.fabric, meta.forge, meta.neoforge];
        if kinds.iter().filter(|&&has| has).count() != 1 {
            continue;
        }
        for (vote, has) in votes.iter_mut().zip(kinds) {
            if has {
                vote.1 += 1;
            }
        }
    }
    votes.sort_by_key(|vote| std::cmp::Reverse(vote.1));
    (votes[0].1 > 0 && votes[0].1 > votes[1].1).then_some(votes[0].0)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Entry {
    pub loader: Loader,
    /// Game versions this file works with (`["1.21.1"]`, `["26.3"]`).
    pub mc: Vec<String>,
    pub version: String,
    pub file: String,
    pub url: String,
    pub size: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    #[serde(default)]
    pub mods: Vec<Entry>,
}

impl Manifest {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let manifest: Self = serde_json::from_slice(data).context(tr!(
            "манифест Phase Companion не читается",
            "the Phase Companion manifest cannot be read"
        ))?;
        if manifest.schema != SCHEMA {
            bail!(
                "{}",
                tr!(
                    "манифест Phase Companion новее этой версии Phase",
                    "the Phase Companion manifest is newer than this version of Phase"
                )
            );
        }
        Ok(manifest)
    }

    /// A suitable entry: own loader, game version in the list. If there are several, the
    /// last one in the file is taken.
    pub fn pick(&self, loader: Loader, mc: &str) -> Option<&Entry> {
        self.mods
            .iter()
            .rev()
            .find(|entry| entry.loader == loader && entry.mc.iter().any(|v| v == mc))
    }
}

/// Where to install from. By default the mod repository's releases; in tests, our own server.
#[derive(Debug, Clone)]
pub struct Source {
    pub manifest_url: String,
    pub allowed_prefixes: Vec<String>,
}

impl Default for Source {
    fn default() -> Self {
        Self {
            manifest_url: MANIFEST_URL.to_string(),
            allowed_prefixes: ALLOWED_PREFIXES.iter().map(ToString::to_string).collect(),
        }
    }
}

/// Installation result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// The needed file is already in place.
    UpToDate { file: String },
    /// The file was installed. `restart`: the game is running and will pick up the mod only after
    /// a restart. `removed`: previous versions of the mod that were replaced.
    Installed {
        file: String,
        version: String,
        restart: bool,
        removed: Vec<String>,
    },
    /// There is no mod build for this loader and game version yet.
    NoBuild,
    /// Could not tell which loader the host has (no Fabric/Forge/NeoForge).
    UnknownLoader,
    /// The game version is unknown: open a world, Phase will see it itself.
    UnknownGame,
    /// The mods folder was not found.
    NoModsFolder,
}

/// Mod files installed in the folder.
fn present(mods_dir: &Path) -> Vec<String> {
    mc_mods::list_jars(mods_dir)
        .map(|jars| {
            jars.into_iter()
                .map(|jar| jar.file_name)
                .filter(|name| mc_mods::is_companion_jar(name))
                .collect()
        })
        .unwrap_or_default()
}

fn client() -> Result<reqwest::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .user_agent(concat!("Phase/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .context(tr!(
            "не удалось подготовить подключение",
            "could not set up the connection"
        ))
}

async fn fetch_manifest(http: &reqwest::Client, source: &Source) -> Result<Manifest> {
    if !source
        .allowed_prefixes
        .iter()
        .any(|p| source.manifest_url.starts_with(p.as_str()))
    {
        bail!(
            "{}",
            tr!(
                "адрес манифеста не разрешён",
                "the manifest address is not allowed"
            )
        );
    }
    let mut response = http
        .get(&source.manifest_url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|err| {
            anyhow::anyhow!(
                "{}: {}",
                tr!("манифест не получен", "could not get the manifest"),
                err.without_url()
            )
        })?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|err| {
        anyhow::anyhow!(
            "{}: {}",
            tr!(
                "манифест оборвался",
                "the manifest download was interrupted"
            ),
            err.without_url()
        )
    })? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_MANIFEST {
            bail!(
                "{}",
                tr!("манифест слишком большой", "the manifest is too large")
            );
        }
    }
    Manifest::parse(&body)
}

async fn download(http: &reqwest::Client, entry: &Entry, part: &Path) -> Result<()> {
    if entry.size == 0 || entry.size > MAX_JAR {
        bail!(
            "{}",
            tr!(
                "в манифесте неверный размер файла",
                "the manifest has a wrong file size"
            )
        );
    }
    let mut response = http
        .get(&entry.url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|err| {
            anyhow::anyhow!(
                "{}: {}",
                tr!("файл мода не получен", "could not get the mod file"),
                err.without_url()
            )
        })?;
    let mut file = tokio::fs::File::create(part).await.with_context(|| {
        tr!(
            format!("не удалось создать {}", part.display()),
            format!("could not create {}", part.display())
        )
    })?;
    let mut have = 0u64;
    while let Some(chunk) = response.chunk().await.map_err(|err| {
        anyhow::anyhow!(
            "{}: {}",
            tr!("загрузка оборвалась", "the download was interrupted"),
            err.without_url()
        )
    })? {
        have += chunk.len() as u64;
        if have > entry.size {
            bail!(
                "{}",
                tr!(
                    "файл мода больше, чем в манифесте",
                    "the mod file is larger than the manifest says"
                )
            );
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    if have != entry.size {
        bail!(
            "{}",
            tr!(
                "файл мода скачан не полностью",
                "the mod file was not fully downloaded"
            )
        );
    }
    let path = part.to_path_buf();
    let (_, hashes) = tokio::task::spawn_blocking(move || mc_mods::hash_file(&path))
        .await
        .context(tr!(
            "проверка мода аварийно завершилась",
            "mod verification crashed"
        ))??;
    if hashes.blake3 != entry.blake3 {
        bail!(
            "{}",
            tr!(
                "файл мода не совпал с контрольной суммой",
                "the mod file does not match its checksum"
            )
        );
    }
    Ok(())
}

/// Installs Phase Companion where the host's game is: finds the mods folder (the chosen one or
/// the running game's), the loader and the version; [`ensure_installed`] does the rest.
pub async fn install_for_host(
    source: &Source,
    chosen_mods_dir: Option<&Path>,
    game_version: Option<&str>,
) -> Result<Outcome> {
    let Some(dir) = mc_mods::locate(chosen_mods_dir, true) else {
        return Ok(Outcome::NoModsFolder);
    };
    let Some(loader) = detect_loader(&dir.path) else {
        return Ok(Outcome::UnknownLoader);
    };
    let Some(mc) = game_version.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(Outcome::UnknownGame);
    };
    ensure_installed(source, &dir.path, mc, loader, mc_mods::is_game_running()).await
}

/// Installs or updates Phase Companion in `mods_dir`.
pub async fn ensure_installed(
    source: &Source,
    mods_dir: &Path,
    mc: &str,
    loader: Loader,
    game_running: bool,
) -> Result<Outcome> {
    let http = client()?;
    let manifest = fetch_manifest(&http, source).await?;
    let Some(entry) = manifest.pick(loader, mc) else {
        return Ok(Outcome::NoBuild);
    };
    mc_mods::validate_file_name(&entry.file)?;
    if !mc_mods::is_companion_jar(&entry.file) {
        bail!(
            "{}",
            tr!(
                "в манифесте файл не похож на Phase Companion",
                "the manifest file does not look like Phase Companion"
            )
        );
    }
    if !source
        .allowed_prefixes
        .iter()
        .any(|prefix| entry.url.starts_with(prefix.as_str()))
    {
        bail!(
            "{}",
            tr!(
                "адрес файла мода не разрешён",
                "the mod file address is not allowed"
            )
        );
    }

    let current = present(mods_dir);
    let target = mods_dir.join(&entry.file);
    if current.iter().any(|name| name == &entry.file)
        && mc_mods::hash_file(&target).is_ok_and(|(_, h)| h.blake3 == entry.blake3)
    {
        return Ok(Outcome::UpToDate {
            file: entry.file.clone(),
        });
    }

    let part: PathBuf = mc_mods::part_path(mods_dir, &entry.file)?;
    if let Err(err) = download(&http, entry, &part).await {
        let _ = fs::remove_file(&part);
        return Err(err);
    }
    if let Err(err) = mc_mods::install(mods_dir, &entry.file) {
        let _ = fs::remove_file(&part);
        return Err(err);
    }
    // Previous mod versions (a different file name) are removed: the game will not start two mods with the same
    // ID.
    let mut removed = Vec::new();
    for old in current.into_iter().filter(|name| name != &entry.file) {
        if fs::remove_file(mods_dir.join(&old)).is_ok() {
            removed.push(old);
        }
    }
    Ok(Outcome::Installed {
        file: entry.file.clone(),
        version: entry.version.clone(),
        restart: game_running,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    fn entry(url: &str, data: &[u8]) -> serde_json::Value {
        serde_json::json!({
            "loader": "fabric", "mc": ["26.3"], "version": "0.1.0",
            "file": "phase-companion-fabric-26.3-0.1.0.jar", "url": url,
            "size": data.len(), "blake3": mc_mods::hash_bytes(data).blake3,
        })
    }

    #[test]
    fn manifest_picks_by_loader_and_version() {
        let manifest = Manifest::parse(
            serde_json::json!({ "schema": 1, "mods": [entry("https://x/y.jar", b"a")] })
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        assert!(manifest.pick(Loader::Fabric, "26.3").is_some());
        assert!(manifest.pick(Loader::Fabric, "1.21.1").is_none());
        assert!(manifest.pick(Loader::Forge, "26.3").is_none());

        // A release with two jars: each game version gets its own file.
        let mut old = entry("https://x/old.jar", b"b");
        old["mc"] = serde_json::json!(["1.21.1"]);
        old["file"] = "phase-companion-fabric-1.21.1-0.1.0.jar".into();
        let both = Manifest::parse(
            serde_json::json!({ "schema": 1, "mods": [old, entry("https://x/y.jar", b"a")] })
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        let file = |mc| both.pick(Loader::Fabric, mc).map(|e| e.file.as_str());
        assert_eq!(
            file("1.21.1"),
            Some("phase-companion-fabric-1.21.1-0.1.0.jar")
        );
        assert_eq!(file("26.3"), Some("phase-companion-fabric-26.3-0.1.0.jar"));
        assert_eq!(file("1.21"), None);

        // A full release: on 1.21.1 three loaders, each gets its own jar.
        let jar = |loader: &str, mc: &str| {
            let mut e = entry(&format!("https://x/{loader}-{mc}.jar"), b"c");
            e["loader"] = loader.into();
            e["mc"] = serde_json::json!([mc]);
            e["file"] = format!("phase-companion-{loader}-{mc}-0.1.0.jar").into();
            e
        };
        let all = Manifest::parse(
            serde_json::json!({ "schema": 1, "mods": [
                jar("fabric", "1.21.1"), jar("fabric", "26.3"),
                jar("forge", "1.21.1"), jar("neoforge", "1.21.1"),
            ] })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        let file = |loader, mc| all.pick(loader, mc).map(|e| e.file.as_str());
        assert_eq!(
            file(Loader::Forge, "1.21.1"),
            Some("phase-companion-forge-1.21.1-0.1.0.jar")
        );
        assert_eq!(
            file(Loader::Neoforge, "1.21.1"),
            Some("phase-companion-neoforge-1.21.1-0.1.0.jar")
        );
        assert_eq!(
            file(Loader::Fabric, "1.21.1"),
            Some("phase-companion-fabric-1.21.1-0.1.0.jar")
        );
        assert_eq!(file(Loader::Forge, "26.3"), None);
        assert_eq!(file(Loader::Neoforge, "26.3"), None);
        assert_eq!(file(Loader::Forge, "1.20.1"), None);
        assert!(Manifest::parse(br#"{"schema":2}"#).is_err());
        assert!(Manifest::parse(b"not json").is_err());
    }

    #[test]
    fn detects_fabric_by_dot_fabric_folder() {
        let dir = tempdir();
        fs::create_dir_all(dir.join(".fabric")).unwrap();
        fs::create_dir_all(dir.join("mods")).unwrap();
        assert_eq!(detect_loader(&dir.join("mods")), Some(Loader::Fabric));
        let empty = tempdir();
        fs::create_dir_all(empty.join("mods")).unwrap();
        assert_eq!(detect_loader(&empty.join("mods")), None);
    }

    #[test]
    fn loader_from_game_command_line() {
        // This is how TLauncher starts Forge 52.1.21 on 1.21.1 (abridged).
        let forge = r#"javaw.exe -cp C:\g\libraries\net\minecraftforge\fmlloader\1.21.1-52.1.21\fmlloader.jar cpw.mods.bootstraplauncher.BootstrapLauncher --launchTarget forge_client --fml.forgeVersion 52.1.21 --fml.mcVersion 1.21.1 --gameDir C:\g --version "Forge 1.21.1""#;
        assert_eq!(loader_from_command_line(forge), Some(Loader::Forge));
        let neo = r"javaw.exe -cp C:\g\libraries\net\neoforged\fancymodloader\loader\4.0.42\loader.jar cpw.mods.bootstraplauncher.BootstrapLauncher --launchTarget forgeclient --fml.neoForgeVersion 21.1.266 --fml.fmlVersion 4.0.42 --gameDir C:\g";
        assert_eq!(loader_from_command_line(neo), Some(Loader::Neoforge));
        let fabric = r"javaw.exe -cp C:\g\libraries\net\fabricmc\fabric-loader\0.17.3\fabric-loader.jar net.fabricmc.loader.impl.launch.knot.KnotClient --gameDir C:\g --version fabric-loader-0.17.3-1.21.1";
        assert_eq!(loader_from_command_line(fabric), Some(Loader::Fabric));
        // Only the version name in the launcher.
        assert_eq!(
            loader_from_command_line(
                r#"javaw.exe x --gameDir C:\g --version "1.21.1-forge-52.1.21""#
            ),
            Some(Loader::Forge)
        );
        assert_eq!(
            loader_from_command_line(
                r"javaw.exe net.minecraft.client.main.Main --gameDir C:\g --version 1.21.1"
            ),
            None
        );
    }

    #[test]
    fn loader_from_mod_metadata() {
        use mc_mods::JarMetadata as M;
        let fabric = M {
            fabric: true,
            ..M::default()
        };
        let forge = M {
            forge: true,
            ..M::default()
        };
        let neo = M {
            neoforge: true,
            ..M::default()
        };
        let multi = M {
            fabric: true,
            forge: true,
            neoforge: true,
        };
        assert_eq!(
            loader_from_mods(&[forge, forge, fabric]),
            Some(Loader::Forge)
        );
        assert_eq!(
            loader_from_mods(&[neo, multi, multi]),
            Some(Loader::Neoforge)
        );
        assert_eq!(loader_from_mods(&[fabric, forge]), None);
        assert_eq!(loader_from_mods(&[multi]), None);
        assert_eq!(loader_from_mods(&[]), None);
    }

    /// Shared TLauncher folder: Fabric was launched earlier (`.fabric` remained), now Forge.
    #[test]
    fn running_forge_beats_leftover_dot_fabric() {
        let forge = mc_mods::JarMetadata {
            forge: true,
            ..Default::default()
        };
        let line = r"javaw.exe --fml.forgeVersion 52.1.21 --gameDir C:\g";
        assert_eq!(pick_loader(Some(line), &[], true), Some(Loader::Forge));
        // The game is not running: mods decide, not the `.fabric` folder.
        assert_eq!(pick_loader(None, &[forge], true), Some(Loader::Forge));
        assert_eq!(pick_loader(None, &[], true), Some(Loader::Fabric));
        assert_eq!(pick_loader(None, &[], false), None);
    }

    fn tempdir() -> PathBuf {
        // A counter, not just time: tests run in parallel, and two calls in the same clock
        // tick got the same folder (install tests occasionally failed).
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "phase-companion-test-{}-{n}-{}",
            std::process::id(),
            mc_mods::hash_bytes(format!("{:?}", std::time::Instant::now()).as_bytes()).blake3
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const CRLF: &str = "\r\n";

    /// Mini server: `/companion.json` and `/mod.jar`. Returns the server address.
    async fn serve(listener: TcpListener, manifest: String, jar: Vec<u8>) {
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let (manifest, jar) = (manifest.clone(), jar.clone());
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]);
                    let body: Vec<u8> = if head.starts_with("GET /mod.jar") {
                        jar
                    } else {
                        manifest.into_bytes()
                    };
                    let head = format!(
                        "HTTP/1.1 200 OK{CRLF}content-length: {}{CRLF}connection: close{CRLF}{CRLF}",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                });
            }
        });
    }

    async fn setup(jar_in_manifest: &[u8], served_jar: &[u8]) -> (Source, PathBuf) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let manifest = serde_json::json!({
            "schema": 1,
            "mods": [entry(&format!("{base}/mod.jar"), jar_in_manifest)],
        })
        .to_string();
        serve(listener, manifest, served_jar.to_vec()).await;
        let source = Source {
            manifest_url: format!("{base}/companion.json"),
            allowed_prefixes: vec![base],
        };
        let mods = tempdir().join("mods");
        fs::create_dir_all(&mods).unwrap();
        (source, mods)
    }

    #[tokio::test]
    async fn installs_then_reports_up_to_date_and_replaces_old_version() {
        let jar = b"PK-fake-jar-bytes".to_vec();
        let (source, mods) = setup(&jar, &jar).await;
        fs::write(mods.join("phase-companion-fabric-26.3-0.0.9.jar"), b"old").unwrap();
        fs::write(mods.join("sodium.jar"), b"other").unwrap();

        let first = ensure_installed(&source, &mods, "26.3", Loader::Fabric, true)
            .await
            .unwrap();
        assert_eq!(
            first,
            Outcome::Installed {
                file: "phase-companion-fabric-26.3-0.1.0.jar".into(),
                version: "0.1.0".into(),
                restart: true,
                removed: vec!["phase-companion-fabric-26.3-0.0.9.jar".into()],
            }
        );
        assert_eq!(
            fs::read(mods.join("phase-companion-fabric-26.3-0.1.0.jar")).unwrap(),
            jar
        );
        assert!(
            mods.join("sodium.jar").exists(),
            "other people's mods are not touched"
        );
        assert!(!mods.join("phase-companion-fabric-26.3-0.0.9.jar").exists());

        let second = ensure_installed(&source, &mods, "26.3", Loader::Fabric, false)
            .await
            .unwrap();
        assert!(matches!(second, Outcome::UpToDate { .. }));
    }

    #[tokio::test]
    async fn refuses_a_file_with_a_wrong_hash_and_leaves_no_trace() {
        let (source, mods) = setup(b"expected-bytes", b"tampered-bytes").await;
        let err = ensure_installed(&source, &mods, "26.3", Loader::Fabric, false).await;
        assert!(err.is_err());
        let left: Vec<_> = fs::read_dir(&mods).unwrap().collect();
        assert!(left.is_empty(), "nothing is left in mods after a refusal");
    }

    /// The real GitHub: `cargo test -p phase-core live_release -- --ignored`.
    #[tokio::test]
    #[ignore = "uses the network"]
    async fn live_release_installs() {
        for (mc, loader) in [
            ("26.3", Loader::Fabric),
            ("1.21.1", Loader::Fabric),
            ("1.21.1", Loader::Forge),
            ("1.21.1", Loader::Neoforge),
        ] {
            let mods = tempdir().join("mods");
            fs::create_dir_all(&mods).unwrap();
            let done = ensure_installed(&Source::default(), &mods, mc, loader, false)
                .await
                .unwrap();
            assert!(
                matches!(done, Outcome::Installed { .. }),
                "{mc} {loader:?}: {done:?}"
            );
            let again = ensure_installed(&Source::default(), &mods, mc, loader, false)
                .await
                .unwrap();
            assert!(
                matches!(again, Outcome::UpToDate { .. }),
                "{mc} {loader:?}: {again:?}"
            );
        }
    }

    #[tokio::test]
    async fn no_build_for_unknown_version_and_foreign_address_is_refused() {
        let jar = b"x".to_vec();
        let (mut source, mods) = setup(&jar, &jar).await;
        let none = ensure_installed(&source, &mods, "1.8.9", Loader::Fabric, false)
            .await
            .unwrap();
        assert_eq!(none, Outcome::NoBuild);
        source.allowed_prefixes = vec!["https://github.com/".into()];
        assert!(
            ensure_installed(&source, &mods, "26.3", Loader::Fabric, false)
                .await
                .is_err()
        );
    }
}

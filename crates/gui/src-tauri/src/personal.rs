//! Window personalization: a custom background.
//!
//! Background: the window shrinks and compresses the picture itself (canvas → WebP) and sends the finished bytes;
//! here they are validated and put into `%LOCALAPPDATA%\Phase\backgrounds\custom.webp`.
//! The player's source file isn't read again and may be deleted.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use phase_tunnel::identity;

type CmdResult<T> = Result<T, String>;

fn to_message(err: anyhow::Error) -> String {
    format!("{err:#}")
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|err| err.to_string())?
        .map_err(to_message)
}

fn data_dir() -> Result<PathBuf> {
    identity::data_dir()
}

// ------------------------------------------------------------------ window background

/// A finished background larger than this is not accepted (the window compresses to ~0.3–1.5 MB).
const MAX_BACKGROUND: usize = 6 * 1024 * 1024;

fn background_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("backgrounds").join("custom.webp"))
}

/// Saves the custom background (WebP, already shrunk by the window). The old one is replaced.
#[tauri::command]
pub async fn save_background(request: tauri::ipc::Request<'_>) -> CmdResult<()> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err(phase_lang::tr!(
            "фон пришёл в странном виде",
            "the background arrived in an odd form"
        )
        .into());
    };
    let bytes = bytes.clone();
    blocking(move || {
        if bytes.len() > MAX_BACKGROUND {
            bail!(phase_lang::tr!(
                "картинка слишком большая",
                "the picture is too large"
            ));
        }
        if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
            bail!(phase_lang::tr!(
                "это не картинка WebP",
                "this is not a WebP picture"
            ));
        }
        let file = background_file()?;
        fs::create_dir_all(file.parent().expect("у файла фона есть папка")).context(
            phase_lang::tr!(
                "не удалось создать папку фона",
                "could not create the background folder"
            ),
        )?;
        let temp = file.with_extension("webp.part");
        fs::write(&temp, &bytes).context(phase_lang::tr!(
            "не удалось сохранить фон",
            "could not save the background"
        ))?;
        fs::rename(&temp, &file).context(phase_lang::tr!(
            "не удалось сохранить фон",
            "could not save the background"
        ))?;
        Ok(())
    })
    .await
}

/// The custom background (WebP bytes). Empty — there is no custom background.
#[tauri::command]
pub async fn load_background() -> CmdResult<tauri::ipc::Response> {
    let bytes = blocking(|| {
        Ok(background_file()
            .ok()
            .and_then(|f| fs::read(f).ok())
            .unwrap_or_default())
    })
    .await?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Deletes the custom background.
#[tauri::command]
pub async fn delete_background() -> CmdResult<()> {
    blocking(|| {
        let file = background_file()?;
        match fs::remove_file(&file) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).context(phase_lang::tr!(
                "не удалось удалить фон",
                "could not delete the background"
            )),
        }
    })
    .await
}

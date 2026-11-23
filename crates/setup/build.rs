//! Installer build: compresses `Phase.exe` into a payload and describes it in
//! `payload_meta.rs`; then the usual Tauri build with an `asInvoker` manifest.
//!
//! The path to the program is the `PHASE_SETUP_PAYLOAD` variable (set by `tools/build-setup.ps1`).
//! Without it an installer without a payload is built: it honestly reports that the file is corrupted,
//! but `cargo build --workspace` and checks don't depend on building the Phase window.

use std::{env, fmt::Write as _, fs, io::Write as _, path::PathBuf};

use sha2::{Digest, Sha256};

/// Manifest: no administrator rights, modern Windows controls, crisp text.
/// An explicit `requestedExecutionLevel` also disables the Windows heuristic that asks for UAC
/// for programs with the word "setup" in the name.
const MANIFEST: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Phase.Setup" version="1.0.0.0" processorArchitecture="*"/>
  <description>Phase Setup</description>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
"#;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn main() {
    println!("cargo:rerun-if-env-changed=PHASE_SETUP_PAYLOAD");
    println!("cargo:rerun-if-env-changed=PHASE_SETUP_BROTLI_QUALITY");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let packed_path = out_dir.join("payload.br");

    let source = env::var_os("PHASE_SETUP_PAYLOAD").map(PathBuf::from);
    let (raw_size, raw_hash, packed) = match &source {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", path.display());
            let raw = fs::read(path)
                .unwrap_or_else(|err| panic!("PHASE_SETUP_PAYLOAD: {}: {err}", path.display()));
            // In a debug build the script isn't optimized: compress quickly.
            let default_quality = if env::var("PROFILE").as_deref() == Ok("debug") {
                1
            } else {
                11
            };
            let quality = env::var("PHASE_SETUP_BROTLI_QUALITY")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(default_quality);
            let mut packed = Vec::with_capacity(raw.len() / 2);
            {
                let mut writer = brotli::CompressorWriter::new(&mut packed, 1 << 16, quality, 24);
                writer.write_all(&raw).expect("сжатие Phase.exe");
                writer.flush().expect("сжатие Phase.exe");
            }
            (raw.len() as u64, Sha256::digest(&raw).to_vec(), packed)
        }
        None => (0, vec![0; 32], Vec::new()),
    };
    fs::write(&packed_path, &packed).expect("запись payload.br");

    let meta = format!(
        "pub const PAYLOAD_PRESENT: bool = {present};\n\
         pub const PACKED: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/payload.br\"));\n\
         pub const PACKED_SHA256: &str = \"{packed_hash}\";\n\
         pub const RAW_SHA256: &str = \"{raw_hash}\";\n\
         pub const RAW_SIZE: u64 = {raw_size};\n",
        present = source.is_some(),
        packed_hash = hex(&Sha256::digest(&packed)),
        raw_hash = hex(&raw_hash),
    );
    fs::write(out_dir.join("payload_meta.rs"), meta).expect("запись payload_meta.rs");

    let windows = tauri_build::WindowsAttributes::new()
        .window_icon_path("icons/icon.ico")
        .app_manifest(MANIFEST);
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("сборка Tauri");
}

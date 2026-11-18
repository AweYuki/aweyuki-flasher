use crate::error::{FlashError, Result};
use crate::reader::SourceReader;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SideloadMetadata {
    pub ota_type: Option<String>,
    pub pre_device: Option<String>,
    pub post_build: Option<String>,
    pub post_timestamp: Option<i64>,
    pub pre_build_incremental: Option<String>,
    pub has_updater_script: bool,
    pub has_payload_bin: bool,
    pub raw_metadata_lines: BTreeMap<String, String>,
    pub total_zip_size: u64,
}

pub fn inspect_sideload_zip(archive_reader: SourceReader) -> Result<Option<SideloadMetadata>> {
    let total_zip_size = archive_reader.len();
    let mut archive = ZipArchive::new(archive_reader)?;

    let mut has_updater_script = false;
    let mut has_payload_bin = false;
    let mut metadata_text: Option<String> = None;

    for i in 0..archive.len() {
        let mut f = archive.by_index(i)?;
        let name = f.name().to_string();
        if name == "META-INF/com/android/metadata" {
            let mut s = String::new();
            let _ = f.read_to_string(&mut s);
            metadata_text = Some(s);
        } else if name == "META-INF/com/google/android/updater-script"
            || name == "META-INF/com/google/android/update-binary"
        {
            has_updater_script = true;
        } else if name == "payload.bin" {
            has_payload_bin = true;
        }
    }

    if metadata_text.is_none() && !has_updater_script {
        return Ok(None);
    }

    let mut map = BTreeMap::new();
    if let Some(txt) = &metadata_text {
        for line in txt.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }

    let post_timestamp = map
        .get("post-timestamp")
        .and_then(|v| v.parse::<i64>().ok());

    Ok(Some(SideloadMetadata {
        ota_type: map.get("ota-type").cloned(),
        pre_device: map.get("pre-device").cloned(),
        post_build: map.get("post-build").cloned(),
        post_timestamp,
        pre_build_incremental: map.get("pre-build-incremental").cloned(),
        has_updater_script,
        has_payload_bin,
        raw_metadata_lines: map,
        total_zip_size,
    }))
}

pub fn validate_sideload_for_device(
    meta: &SideloadMetadata,
    device_codename: Option<&str>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if let (Some(pre_dev), Some(dev)) = (&meta.pre_device, device_codename) {
        let allowed: Vec<&str> = pre_dev.split(',').map(|s| s.trim()).collect();
        if !allowed.iter().any(|&a| a.eq_ignore_ascii_case(dev)) {
            warnings.push(format!(
                "Device mismatch: OTA expects '{pre_dev}', connected device is '{dev}'"
            ));
        }
    }
    if meta.ota_type.as_deref() == Some("BLOCK") && !meta.has_payload_bin {
        warnings.push("Recovery BLOCK OTA detected (requires adb sideload in recovery).".into());
    }
    warnings
}

/// Build a valid Recovery Sideload OTA `.zip` for demo & test purposes.
pub fn build_sample_sideload_zip(
    codename: &str,
    build_id: &str,
    payload_bytes: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zw = ZipWriter::new(&mut buf);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        let meta_content = format!(
            "ota-type={}\npre-device={}\npost-build=aweyuki/{}/{}:15/{}/release-keys\npost-timestamp=1760000000\n",
            if payload_bytes.is_some() { "AB" } else { "BLOCK" },
            codename,
            codename,
            codename,
            build_id
        );
        zw.start_file("META-INF/com/android/metadata", deflated)?;
        zw.write_all(meta_content.as_bytes())?;

        zw.start_file("META-INF/com/google/android/updater-script", deflated)?;
        zw.write_all(
            b"ui_print(\"Installing Aweyuki Demo ROM...\");\nshow_progress(1.0, 0);\n",
        )?;

        zw.start_file("META-INF/com/google/android/update-binary", deflated)?;
        zw.write_all(b"#!/sbin/sh\n# Aweyuki demo update-binary stub\nexit 0\n")?;

        if let Some(payload) = payload_bytes {
            zw.start_file("payload.bin", stored)?;
            zw.write_all(payload)?;
            zw.start_file("payload_properties.txt", deflated)?;
            zw.write_all(b"POWERWASH=0\nSWITCH_SLOT_ON_REBOOT=1\n")?;
        } else {
            // Legacy block-based OTA assets
            zw.start_file("system.transfer.list", deflated)?;
            zw.write_all(b"4\n2\n0\n0\n")?;
            zw.start_file("system.new.dat.br", stored)?;
            zw.write_all(&vec![0xA5u8; 32 * 1024])?;
            zw.start_file("boot.img", stored)?;
            let mut boot = vec![0u8; 16 * 1024];
            boot[0..8].copy_from_slice(b"ANDROID!");
            zw.write_all(&boot)?;
        }

        zw.finish().map_err(FlashError::Zip)?;
    }
    Ok(buf.into_inner())
}

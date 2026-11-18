use anyhow::{anyhow, Result as AnyResult};
use aweyuki_core::error::{FlashError, Result};
use aweyuki_core::planner::{DeviceMode, DeviceTargetInfo};
use aweyuki_core::simulator::FlashTransport;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    pub info: DeviceTargetInfo,
    pub connection: String,
}

#[derive(Debug, Clone)]
pub struct NativeCliTransport {
    pub serial: Option<String>,
    pub fastboot_bin: String,
    pub adb_bin: String,
    pub is_termux: bool,
    pub termux_usb_dev: Option<String>,
    pub temp_seq: u64,
}

impl NativeCliTransport {
    pub fn detect(serial: Option<String>, prefer_termux_usb: bool) -> Self {
        let is_termux = std::env::var_os("TERMUX_VERSION").is_some()
            || std::env::var("PREFIX")
                .map(|p| p.contains("com.termux"))
                .unwrap_or(false);

        let fastboot_bin = resolve_binary(&["termux-fastboot", "fastboot"], is_termux)
            .unwrap_or_else(|| "fastboot".to_string());
        let adb_bin = resolve_binary(&["termux-adb", "adb"], is_termux)
            .unwrap_or_else(|| "adb".to_string());

        let termux_usb_dev = if prefer_termux_usb || is_termux {
            query_termux_usb_devices().into_iter().next()
        } else {
            None
        };

        Self {
            serial,
            fastboot_bin,
            adb_bin,
            is_termux,
            termux_usb_dev,
            temp_seq: 0,
        }
    }

    fn fastboot_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.fastboot_bin);
        if let Some(s) = &self.serial {
            cmd.arg("-s").arg(s);
        }
        cmd.args(args);
        cmd
    }

    fn adb_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.adb_bin);
        if let Some(s) = &self.serial {
            cmd.arg("-s").arg(s);
        }
        cmd.args(args);
        cmd
    }

    fn run_streaming(&self, mut cmd: Command, log: &mut dyn FnMut(&str)) -> Result<String> {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| FlashError::Transport(format!("Failed to launch command: {e}")))?;

        let mut combined = String::new();
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
                if !line.trim().is_empty() {
                    log(&format!("  {line}"));
                }
                combined.push_str(&line);
                combined.push('\n');
            }
        }
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
                if !line.trim().is_empty() {
                    log(&format!("  {line}"));
                }
                combined.push_str(&line);
                combined.push('\n');
            }
        }

        let status = child
            .wait()
            .map_err(|e| FlashError::Transport(format!("Wait failed: {e}")))?;
        if !status.success() {
            return Err(FlashError::Transport(format!(
                "Command exited with {status}: {}",
                combined.trim()
            )));
        }
        Ok(combined)
    }

    fn write_temp_blob(&mut self, prefix: &str, data: &[u8]) -> Result<PathBuf> {
        self.temp_seq += 1;
        let path = std::env::temp_dir().join(format!(
            "aweyuki_{}_{}_{}.img",
            std::process::id(),
            prefix,
            self.temp_seq
        ));
        std::fs::write(&path, data)?;
        Ok(path)
    }
}

impl FlashTransport for NativeCliTransport {
    fn query_device(&mut self) -> Result<DeviceTargetInfo> {
        let discovered = discover_real_devices(self.is_termux);
        if let Some(serial) = &self.serial {
            if let Some(found) = discovered.iter().find(|d| &d.info.serial == serial) {
                return Ok(found.info.clone());
            }
        }
        if let Some(first) = discovered.into_iter().next() {
            self.serial = Some(first.info.serial.clone());
            return Ok(first.info);
        }
        Err(FlashError::Transport(
            "No Fastboot/ADB device detected. Connect device via USB (or pass --simulate)."
                .into(),
        ))
    }

    fn flash_blob(
        &mut self,
        partition: &str,
        data: &[u8],
        disable_verity: bool,
        disable_verification: bool,
        log: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let tmp_path = self.write_temp_blob(partition, data)?;
        let path_str = tmp_path.to_string_lossy().to_string();
        let mut args: Vec<&str> = Vec::new();
        if disable_verity {
            args.push("--disable-verity");
        }
        if disable_verification {
            args.push("--disable-verification");
        }
        args.push("flash");
        args.push(partition);
        args.push(&path_str);

        log(&format!("> {} {}", self.fastboot_bin, args.join(" ")));
        let res = self.run_streaming(self.fastboot_cmd(&args), log);
        let _ = std::fs::remove_file(&tmp_path);
        res.map(|_| ())
    }

    fn reboot_fastbootd(&mut self, log: &mut dyn FnMut(&str)) -> Result<()> {
        log(&format!("> {} reboot fastboot", self.fastboot_bin));
        self.run_streaming(self.fastboot_cmd(&["reboot", "fastboot"]), log)?;
        Ok(())
    }

    fn delete_logical(&mut self, name: &str, log: &mut dyn FnMut(&str)) -> Result<()> {
        log(&format!("> {} delete-logical-partition {name}", self.fastboot_bin));
        let _ = self.run_streaming(self.fastboot_cmd(&["delete-logical-partition", name]), log);
        Ok(())
    }

    fn create_logical(
        &mut self,
        name: &str,
        size_bytes: u64,
        log: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let sz = size_bytes.to_string();
        log(&format!(
            "> {} create-logical-partition {name} {sz}",
            self.fastboot_bin
        ));
        self.run_streaming(
            self.fastboot_cmd(&["create-logical-partition", name, &sz]),
            log,
        )?;
        Ok(())
    }

    fn resize_logical(
        &mut self,
        name: &str,
        size_bytes: u64,
        log: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let sz = size_bytes.to_string();
        log(&format!(
            "> {} resize-logical-partition {name} {sz}",
            self.fastboot_bin
        ));
        self.run_streaming(
            self.fastboot_cmd(&["resize-logical-partition", name, &sz]),
            log,
        )?;
        Ok(())
    }

    fn set_active_slot(&mut self, slot: &str, log: &mut dyn FnMut(&str)) -> Result<()> {
        log(&format!("> {} set_active {slot}", self.fastboot_bin));
        self.run_streaming(self.fastboot_cmd(&["set_active", slot]), log)?;
        Ok(())
    }

    fn reboot_sideload(&mut self, log: &mut dyn FnMut(&str)) -> Result<()> {
        log(&format!("> {} reboot sideload", self.adb_bin));
        self.run_streaming(self.adb_cmd(&["reboot", "sideload"]), log)?;
        Ok(())
    }

    fn adb_sideload(
        &mut self,
        package_name: &str,
        data: &[u8],
        log: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let tmp_path = self.write_temp_blob("sideload", data)?;
        let path_str = tmp_path.to_string_lossy().to_string();
        log(&format!("> {} sideload {package_name}", self.adb_bin));
        let res = self.run_streaming(self.adb_cmd(&["sideload", &path_str]), log);
        let _ = std::fs::remove_file(&tmp_path);
        res.map(|_| ())
    }

    fn wipe_userdata(&mut self, log: &mut dyn FnMut(&str)) -> Result<()> {
        log(&format!("> {} -w", self.fastboot_bin));
        self.run_streaming(self.fastboot_cmd(&["-w"]), log)?;
        Ok(())
    }

    fn reboot_system(&mut self, log: &mut dyn FnMut(&str)) -> Result<()> {
        log(&format!("> {} reboot", self.fastboot_bin));
        self.run_streaming(self.fastboot_cmd(&["reboot"]), log)?;
        Ok(())
    }
}

fn resolve_binary(candidates: &[&str], is_termux: bool) -> Option<String> {
    let mut search_dirs: Vec<PathBuf> = Vec::new();
    if is_termux {
        if let Some(prefix) = std::env::var_os("PREFIX") {
            search_dirs.push(PathBuf::from(prefix).join("bin"));
        }
    }
    if let Some(sdk) = std::env::var_os("ANDROID_HOME").or_else(|| std::env::var_os("ANDROID_SDK_ROOT")) {
        search_dirs.push(PathBuf::from(sdk).join("platform-tools"));
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        search_dirs.push(PathBuf::from(local).join("Android\\Sdk\\platform-tools"));
    }

    for name in candidates {
        let exe_name = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            (*name).to_string()
        };
        for dir in &search_dirs {
            let candidate = dir.join(&exe_name);
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
        if Command::new(&exe_name)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
        {
            return Some(exe_name);
        }
    }
    None
}

pub fn query_termux_usb_devices() -> Vec<String> {
    let Ok(out) = Command::new("termux-usb").arg("-l").output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim()
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn discover_real_devices(is_termux: bool) -> Vec<DiscoveredDevice> {
    let mut out = Vec::new();
    let fb_bin = resolve_binary(&["termux-fastboot", "fastboot"], is_termux)
        .unwrap_or_else(|| "fastboot".into());
    let adb_bin =
        resolve_binary(&["termux-adb", "adb"], is_termux).unwrap_or_else(|| "adb".into());

    if let Ok(res) = Command::new(&fb_bin).arg("devices").output() {
        if res.status.success() {
            let stdout = String::from_utf8_lossy(&res.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 && (parts[1] == "fastboot" || parts[1] == "fastbootd") {
                    let serial = parts[0].to_string();
                    let info = query_fastboot_vars(&fb_bin, &serial, parts[1] == "fastbootd");
                    out.push(DiscoveredDevice {
                        info,
                        connection: "Fastboot USB".into(),
                    });
                }
            }
        }
    }

    if let Ok(res) = Command::new(&adb_bin).args(["devices", "-l"]).output() {
        if res.status.success() {
            let stdout = String::from_utf8_lossy(&res.stdout);
            for line in stdout.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 && (parts[1] == "device" || parts[1] == "sideload" || parts[1] == "recovery") {
                    let serial = parts[0].to_string();
                    let mode = if parts[1] == "sideload" {
                        DeviceMode::RecoverySideload
                    } else {
                        DeviceMode::Adb
                    };
                    let product = parts
                        .iter()
                        .find_map(|p| p.strip_prefix("product:").or_else(|| p.strip_prefix("device:")))
                        .unwrap_or("android")
                        .to_string();
                    out.push(DiscoveredDevice {
                        info: DeviceTargetInfo {
                            serial,
                            product,
                            mode,
                            current_slot: Some("a".into()),
                            is_ab: true,
                            max_download_size: 256 * 1024 * 1024,
                            existing_logical_partitions: vec![],
                        },
                        connection: "ADB USB".into(),
                    });
                }
            }
        }
    }

    out
}

fn query_fastboot_vars(fb_bin: &str, serial: &str, hinted_fastbootd: bool) -> DeviceTargetInfo {
    let mut info = DeviceTargetInfo {
        serial: serial.to_string(),
        product: "unknown".into(),
        mode: if hinted_fastbootd {
            DeviceMode::Fastbootd
        } else {
            DeviceMode::Bootloader
        },
        current_slot: None,
        is_ab: false,
        max_download_size: 256 * 1024 * 1024,
        existing_logical_partitions: Vec::new(),
    };

    if let Ok(out) = Command::new(fb_bin)
        .args(["-s", serial, "getvar", "all"])
        .output()
    {
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        for line in text.lines() {
            let cleaned = line.trim().trim_start_matches("(bootloader)").trim();
            let Some((k, v)) = cleaned.split_once(':') else {
                continue;
            };
            let key = k.trim();
            let val = v.trim();
            match key {
                "product" => info.product = val.to_string(),
                "current-slot" => {
                    info.is_ab = true;
                    info.current_slot = Some(val.trim_start_matches('_').to_string());
                }
                "slot-count" => {
                    if val.parse::<u32>().unwrap_or(0) >= 2 {
                        info.is_ab = true;
                    }
                }
                "is-userspace" if val == "yes" => info.mode = DeviceMode::Fastbootd,
                "max-download-size" => {
                    if let Some(parsed) = parse_hex_or_dec_u64(val) {
                        info.max_download_size = parsed;
                    }
                }
                _ => {
                    if let Some(part) = key.strip_prefix("is-logical:") {
                        if val == "yes" {
                            info.existing_logical_partitions.push(part.to_string());
                        }
                    }
                }
            }
        }
    }
    info
}

fn parse_hex_or_dec_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

/// Optional helper when invoked inside `termux-usb -r -e`
pub fn run_under_termux_usb_if_requested(usb_dev: &Path, args: &[String]) -> AnyResult<()> {
    let exe = std::env::current_exe()?;
    let status = Command::new("termux-usb")
        .arg("-r")
        .arg("-e")
        .arg(format!("{} {}", exe.display(), args.join(" ")))
        .arg(usb_dev)
        .status()?;
    if !status.success() {
        return Err(anyhow!("termux-usb wrapper exited with {status}"));
    }
    Ok(())
}

use crate::utils::dirs;
use anyhow::{Context as _, Result, bail};
#[cfg(target_os = "windows")]
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use serde_yaml_ng::{Mapping, Value};
#[cfg(target_os = "windows")]
use sha2::{Digest as _, Sha256};
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
use std::ffi::OsStr;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::Write as _;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Stdio;
use tokio::fs;

const SETTINGS_FILE: &str = "openconnect.json";
const PID_FILE: &str = "openconnect.pid";
const LOG_FILE: &str = "openconnect.log";
const CREDENTIAL_TARGET: &str = "ClashVergeRev/OpenConnect/default";
#[cfg(target_os = "macos")]
const CREDENTIAL_ACCOUNT: &str = "default";
#[cfg(target_os = "windows")]
const WINDOWS_INSTALLER_URL: &str =
    "https://www.infradead.org/openconnect-gui/download/openconnect-gui-1.6.0-win64.exe";
#[cfg(target_os = "windows")]
const WINDOWS_INSTALLER_SHA256: &str = "4DBE109C7B72F8F2F4DAF5C311F99D4DD8A2919EEFE01128E60BABFA1DEEC852";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenConnectDiscovery {
    pub platform: String,
    pub executable: Option<String>,
    pub vpnc_script: Option<String>,
    pub installer_available: bool,
    pub credential_store_available: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenConnectSettings {
    pub name: String,
    pub executable: String,
    pub endpoint: String,
    pub protocol: String,
    pub auth_group: String,
    pub username: String,
    pub vpn_interface: String,
    pub vpnc_script: String,
    pub physical_interface: String,
    #[serde(default)]
    pub route_prefixes: Vec<String>,
    #[serde(default)]
    pub direct_domains: Vec<String>,
    #[serde(default)]
    pub dns_servers: Vec<String>,
}

impl OpenConnectSettings {
    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("Tunnel name is required");
        }
        if self.executable.trim().is_empty() || !PathBuf::from(&self.executable).is_file() {
            bail!("OpenConnect executable does not exist");
        }
        if self.endpoint.trim().is_empty() {
            bail!("VPN endpoint is required");
        }
        if self.username.trim().is_empty() {
            bail!("VPN username is required");
        }
        #[cfg(not(target_os = "macos"))]
        if self.vpn_interface.trim().is_empty() {
            bail!("VPN interface name is required");
        }
        if !self.vpnc_script.trim().is_empty() && !PathBuf::from(&self.vpnc_script).is_file() {
            bail!("vpnc script does not exist");
        }
        for prefix in &self.route_prefixes {
            validate_prefix(prefix)?;
        }
        for server in &self.dns_servers {
            server
                .trim()
                .parse::<IpAddr>()
                .with_context(|| format!("invalid campus DNS server: {server}"))?;
        }
        if self
            .direct_domains
            .iter()
            .any(|domain| normalized_domain(domain).is_empty())
        {
            bail!("campus domains cannot be empty");
        }
        Ok(())
    }
}

fn validate_prefix(prefix: &str) -> Result<()> {
    let (address, length) = prefix
        .trim()
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("invalid campus route prefix: {prefix}"))?;
    let address = address
        .parse::<IpAddr>()
        .with_context(|| format!("invalid campus route address: {address}"))?;
    let length = length
        .parse::<u8>()
        .with_context(|| format!("invalid campus route prefix length: {length}"))?;
    let maximum = if address.is_ipv4() { 32 } else { 128 };
    if length > maximum {
        bail!("invalid campus route prefix length: {length}");
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenConnectStatus {
    pub configured: bool,
    pub has_password: bool,
    pub connected: bool,
    pub process_id: Option<u32>,
}

fn tunnel_dir() -> Result<PathBuf> {
    ::dirs::data_local_dir()
        .map(|root| root.join(dirs::APP_ID).join("external-tunnels"))
        .ok_or_else(|| anyhow::anyhow!("Failed to get the local application data directory"))
}

fn settings_path() -> Result<PathBuf> {
    Ok(tunnel_dir()?.join(SETTINGS_FILE))
}

fn pid_path() -> Result<PathBuf> {
    Ok(tunnel_dir()?.join(PID_FILE))
}

fn log_path() -> Result<PathBuf> {
    Ok(tunnel_dir()?.join(LOG_FILE))
}

fn command_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn executable_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Some(path) = command_in_path("openconnect.exe") {
            candidates.push(path);
        }
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(root) = std::env::var_os(variable) {
                let root = PathBuf::from(root);
                candidates.push(root.join("OpenConnect-GUI").join("openconnect.exe"));
                candidates.push(root.join("Programs").join("OpenConnect-GUI").join("openconnect.exe"));
            }
        }
        for root in [r"C:\App", r"C:\Apps", r"D:\App", r"D:\Apps"] {
            candidates.push(Path::new(root).join("OpenConnect-GUI").join("openconnect.exe"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(path) = command_in_path("openconnect") {
            candidates.push(path);
        }
        candidates.extend([
            PathBuf::from("/opt/homebrew/bin/openconnect"),
            PathBuf::from("/usr/local/bin/openconnect"),
            PathBuf::from("/opt/local/sbin/openconnect"),
        ]);
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(path) = command_in_path("openconnect") {
            candidates.push(path);
        }
        candidates.extend([
            PathBuf::from("/usr/bin/openconnect"),
            PathBuf::from("/usr/sbin/openconnect"),
            PathBuf::from("/usr/local/bin/openconnect"),
        ]);
    }
    candidates
}

fn vpnc_script_candidates(executable: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(parent) = executable.and_then(Path::parent) {
        candidates.push(parent.join("vpnc-script.js"));
        candidates.push(parent.join("vpnc-script"));
        if let Some(root) = parent.parent() {
            candidates.push(root.join("share").join("vpnc-scripts").join("vpnc-script"));
            candidates.push(root.join("etc").join("vpnc").join("vpnc-script"));
        }
    }
    #[cfg(target_os = "macos")]
    candidates.extend([
        PathBuf::from("/opt/homebrew/etc/vpnc/vpnc-script"),
        PathBuf::from("/usr/local/etc/vpnc/vpnc-script"),
        PathBuf::from("/opt/local/etc/vpnc/vpnc-script"),
    ]);
    #[cfg(target_os = "linux")]
    candidates.extend([
        PathBuf::from("/usr/share/vpnc-scripts/vpnc-script"),
        PathBuf::from("/etc/vpnc/vpnc-script"),
    ]);
    candidates
}

#[cfg(target_os = "windows")]
const fn installer_available() -> bool {
    true
}

#[cfg(target_os = "macos")]
fn installer_available() -> bool {
    command_in_path("brew").is_some()
        || Path::new("/opt/homebrew/bin/brew").is_file()
        || Path::new("/usr/local/bin/brew").is_file()
}

#[cfg(target_os = "linux")]
fn installer_available() -> bool {
    command_in_path("pkexec").is_some()
        && ["apt-get", "dnf", "pacman", "zypper"]
            .iter()
            .any(|manager| command_in_path(manager).is_some())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const fn installer_available() -> bool {
    false
}

pub async fn discover() -> Result<OpenConnectDiscovery> {
    let stored = load_settings().await?;
    let executable = stored
        .as_ref()
        .map(|settings| PathBuf::from(&settings.executable))
        .filter(|path| path.is_file())
        .or_else(|| executable_candidates().into_iter().find(|path| path.is_file()));
    let vpnc_script = stored
        .as_ref()
        .map(|settings| PathBuf::from(&settings.vpnc_script))
        .filter(|path| path.is_file())
        .or_else(|| {
            vpnc_script_candidates(executable.as_deref())
                .into_iter()
                .find(|path| path.is_file())
        });

    Ok(OpenConnectDiscovery {
        platform: std::env::consts::OS.into(),
        executable: executable.map(|path| path.to_string_lossy().into_owned()),
        vpnc_script: vpnc_script.map(|path| path.to_string_lossy().into_owned()),
        installer_available: installer_available(),
        credential_store_available: credential_store_available(),
    })
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
const fn credential_store_available() -> bool {
    true
}

#[cfg(target_os = "linux")]
fn credential_store_available() -> bool {
    command_in_path("secret-tool").is_some()
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const fn credential_store_available() -> bool {
    false
}

pub async fn install() -> Result<OpenConnectDiscovery> {
    #[cfg(target_os = "windows")]
    install_windows().await?;
    #[cfg(target_os = "macos")]
    install_macos().await?;
    #[cfg(target_os = "linux")]
    install_linux().await?;
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    bail!("Automatic OpenConnect installation is not supported on this platform");

    let discovery = discover().await?;
    if discovery.executable.is_none() {
        bail!("OpenConnect installation completed, but the executable could not be found");
    }
    if !discovery.credential_store_available {
        bail!("OpenConnect installation completed, but secure credential storage is unavailable");
    }
    Ok(discovery)
}

#[cfg(target_os = "windows")]
async fn install_windows() -> Result<()> {
    let download_dir = tunnel_dir()?.join("downloads");
    fs::create_dir_all(&download_dir).await?;
    let installer = download_dir.join("openconnect-gui-1.6.0-win64.exe");
    let needs_download = match fs::read(&installer).await {
        Ok(bytes) => sha256_hex(&bytes) != WINDOWS_INSTALLER_SHA256,
        Err(_) => true,
    };
    if needs_download {
        let response = reqwest::get(WINDOWS_INSTALLER_URL)
            .await
            .context("failed to download the official OpenConnect installer")?
            .error_for_status()
            .context("the official OpenConnect download returned an error")?;
        let bytes = response
            .bytes()
            .await
            .context("failed to read the OpenConnect installer")?;
        if sha256_hex(&bytes) != WINDOWS_INSTALLER_SHA256 {
            bail!("OpenConnect installer checksum verification failed");
        }
        fs::write(&installer, &bytes).await?;
    }

    let result = tokio::task::spawn_blocking(move || runas::Command::new(&installer).show(true).status())
        .await
        .context("OpenConnect installer task failed")??;
    if !result.success() {
        bail!("OpenConnect installer exited with status {result}");
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02X}")).collect()
}

#[cfg(target_os = "macos")]
async fn install_macos() -> Result<()> {
    let brew = command_in_path("brew")
        .or_else(|| {
            ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
                .into_iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
        })
        .ok_or_else(|| anyhow::anyhow!("Homebrew is required to install OpenConnect automatically"))?;
    let status = tokio::process::Command::new(brew)
        .args(["install", "openconnect"])
        .status()
        .await?;
    if !status.success() {
        bail!("Homebrew failed to install OpenConnect");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn install_linux() -> Result<()> {
    let pkexec = command_in_path("pkexec")
        .ok_or_else(|| anyhow::anyhow!("pkexec is required to install OpenConnect automatically"))?;
    let (manager, arguments): (PathBuf, &[&str]) = if let Some(manager) = command_in_path("apt-get") {
        (manager, &["install", "-y", "openconnect", "libsecret-tools"])
    } else if let Some(manager) = command_in_path("dnf") {
        (manager, &["install", "-y", "openconnect", "libsecret"])
    } else if let Some(manager) = command_in_path("pacman") {
        (manager, &["-S", "--needed", "--noconfirm", "openconnect", "libsecret"])
    } else if let Some(manager) = command_in_path("zypper") {
        (
            manager,
            &["--non-interactive", "install", "openconnect", "libsecret-tools"],
        )
    } else {
        bail!("No supported Linux package manager was found");
    };
    let status = tokio::process::Command::new(pkexec)
        .arg(manager)
        .args(arguments)
        .status()
        .await?;
    if !status.success() {
        bail!("The Linux package manager failed to install OpenConnect");
    }
    Ok(())
}

pub async fn load_settings() -> Result<Option<OpenConnectSettings>> {
    let path = settings_path()?;
    match fs::read(&path).await {
        Ok(data) => serde_json::from_slice(&data)
            .map(Some)
            .with_context(|| format!("failed to parse {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub async fn save_settings(settings: &OpenConnectSettings, password: Option<&str>) -> Result<()> {
    settings.validate()?;
    let dir = tunnel_dir()?;
    fs::create_dir_all(&dir).await?;
    let data = serde_json::to_vec_pretty(settings)?;
    fs::write(settings_path()?, data).await?;

    if let Some(password) = password {
        if password.is_empty() {
            delete_password()?;
        } else {
            store_password(&settings.username, password)?;
        }
    }
    Ok(())
}

pub async fn status() -> Result<OpenConnectStatus> {
    let configured = load_settings().await?.is_some();
    let has_password = password_exists()?;
    let process_id = match fs::read_to_string(pid_path()?).await {
        Ok(raw) => raw.trim().parse::<u32>().ok(),
        Err(_) => None,
    };
    let connected = match process_id {
        Some(pid) => process_is_running(pid).await,
        None => false,
    };

    Ok(OpenConnectStatus {
        configured,
        has_password,
        connected,
        process_id: connected.then_some(process_id).flatten(),
    })
}

pub async fn apply_split_routing(config: Mapping) -> Mapping {
    let Ok(Some(settings)) = load_settings().await else {
        return config;
    };
    let Ok(current_status) = status().await else {
        return config;
    };
    if !current_status.connected {
        return config;
    }
    inject_split_routing(config, &settings)
}

fn inject_split_routing(mut config: Mapping, settings: &OpenConnectSettings) -> Mapping {
    if !settings.route_prefixes.is_empty() {
        let mut tun = config
            .remove("tun")
            .and_then(|value| value.as_mapping().cloned())
            .unwrap_or_default();
        prepend_unique_strings(&mut tun, "route-exclude-address", &settings.route_prefixes);
        config.insert(Value::from("tun"), Value::Mapping(tun));
    }

    let direct_rules = settings
        .direct_domains
        .iter()
        .map(|domain| format!("DOMAIN-SUFFIX,{},DIRECT", normalized_domain(domain)))
        .chain(
            settings
                .route_prefixes
                .iter()
                .map(|prefix| format!("IP-CIDR,{prefix},DIRECT,no-resolve")),
        )
        .collect::<Vec<_>>();
    let mut rules = direct_rules.iter().cloned().map(Value::from).collect::<Vec<_>>();
    if let Some(Value::Sequence(existing)) = config.remove("rules") {
        for rule in existing {
            if !rule
                .as_str()
                .is_some_and(|rule| direct_rules.iter().any(|managed| managed == rule))
            {
                rules.push(rule);
            }
        }
    }
    if !rules.is_empty() {
        config.insert(Value::from("rules"), Value::Sequence(rules));
    }

    if !settings.direct_domains.is_empty()
        && !settings.dns_servers.is_empty()
        && let Some(Value::Mapping(dns)) = config.get_mut("dns")
    {
        let policy = dns
            .entry(Value::from("nameserver-policy"))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if let Value::Mapping(policy) = policy {
            let servers = Value::Sequence(settings.dns_servers.iter().cloned().map(Value::from).collect());
            for domain in &settings.direct_domains {
                policy.insert(Value::from(format!("+.{}", normalized_domain(domain))), servers.clone());
            }
        }
    }

    config
}

fn prepend_unique_strings(mapping: &mut Mapping, key: &str, values: &[String]) {
    let mut combined = values.to_vec();
    if let Some(Value::Sequence(existing)) = mapping.remove(key) {
        for value in existing
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
        {
            if !combined.contains(&value) {
                combined.push(value);
            }
        }
    }
    mapping.insert(
        Value::from(key),
        Value::Sequence(combined.into_iter().map(Value::from).collect()),
    );
}

fn normalized_domain(domain: &str) -> &str {
    domain.trim().trim_start_matches("+.").trim_start_matches('.')
}

async fn process_is_running(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        let script = format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}");
        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .creation_flags(CREATE_NO_WINDOW)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        command.status().await.is_ok_and(|status| status.success())
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let pid = match i32::try_from(pid) {
            Ok(pid) => pid,
            Err(_) => return false,
        };
        let result = unsafe { libc::kill(pid, 0) };
        if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
            return false;
        }
        process_is_openconnect(pid).await
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
async fn process_is_openconnect(pid: i32) -> bool {
    fs::read_link(format!("/proc/{pid}/exe"))
        .await
        .ok()
        .and_then(|path| path.file_name().map(|name| name == "openconnect"))
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
async fn process_is_openconnect(pid: i32) -> bool {
    tokio::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|command| Path::new(command.trim()).file_name() == Some(OsStr::new("openconnect")))
}

pub async fn set_connected(enabled: bool) -> Result<OpenConnectStatus> {
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = enabled;
        bail!("OpenConnect orchestration is not supported on this platform");
    }

    #[cfg(target_os = "windows")]
    {
        let settings = load_settings()
            .await?
            .ok_or_else(|| anyhow::anyhow!("OpenConnect is not configured"))?;
        settings.validate()?;
        if enabled && !password_exists()? {
            bail!("OpenConnect password is not saved");
        }

        let helper = dirs::app_resources_dir()?.join("openconnect-helper.ps1");
        if !helper.is_file() {
            bail!("OpenConnect helper is missing: {}", helper.display());
        }

        let action = if enabled { "Connect" } else { "Disconnect" };
        // `runas` 1.2.0 doubles every backslash in quoted arguments on Windows.
        // That corrupts `-File "C:\Program Files\..."` and PowerShell exits with
        // 0xfffd0000 before the helper starts. Encode the invocation so every
        // argument passed through `runas` is free of spaces and backslashes.
        let command = openconnect_helper_command(
            &helper,
            action,
            &settings_path()?,
            &pid_path()?,
            &log_path()?,
            CREDENTIAL_TARGET,
        );
        let encoded_command = encode_powershell_command(&command);
        let args = vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-ExecutionPolicy".to_owned(),
            "Bypass".to_owned(),
            "-EncodedCommand".to_owned(),
            encoded_command,
        ];

        let result =
            tokio::task::spawn_blocking(move || runas::Command::new("powershell.exe").args(&args).show(false).status())
                .await
                .context("OpenConnect helper task failed")??;
        if !result.success() {
            bail!("OpenConnect helper exited with status {result}");
        }

        for _ in 0..20 {
            let current = status().await?;
            if current.connected == enabled {
                return Ok(current);
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        status().await
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        set_connected_unix(enabled).await
    }
}

#[cfg(target_os = "windows")]
fn powershell_single_quote(value: impl AsRef<OsStr>) -> String {
    format!("'{}'", value.as_ref().to_string_lossy().replace('\'', "''"))
}

#[cfg(target_os = "windows")]
fn openconnect_helper_command(
    helper: &Path,
    action: &str,
    settings: &Path,
    pid: &Path,
    log: &Path,
    credential_target: &str,
) -> String {
    format!(
        "& {} -Action {} -SettingsPath {} -PidPath {} -LogPath {} -CredentialTarget {}",
        powershell_single_quote(helper),
        powershell_single_quote(action),
        powershell_single_quote(settings),
        powershell_single_quote(pid),
        powershell_single_quote(log),
        powershell_single_quote(credential_target),
    )
}

#[cfg(target_os = "windows")]
fn encode_powershell_command(command: &str) -> String {
    let utf16_le = command
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect::<Vec<_>>();
    BASE64_STANDARD.encode(utf16_le)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn set_connected_unix(enabled: bool) -> Result<OpenConnectStatus> {
    let settings = load_settings()
        .await?
        .ok_or_else(|| anyhow::anyhow!("OpenConnect is not configured"))?;
    settings.validate()?;

    if enabled {
        if status().await?.connected {
            return status().await;
        }
        let password = load_password()?.ok_or_else(|| anyhow::anyhow!("OpenConnect password is not saved"))?;
        let settings = settings.clone();
        tokio::task::spawn_blocking(move || start_openconnect(&settings, password))
            .await
            .context("OpenConnect launch task failed")??;
    } else if let Some(pid) = status().await?.process_id {
        tokio::task::spawn_blocking(move || stop_openconnect(pid))
            .await
            .context("OpenConnect stop task failed")??;
    } else {
        remove_pid_file()?;
    }

    for _ in 0..40 {
        let current = status().await?;
        if current.connected == enabled {
            return Ok(current);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let current = status().await?;
    if current.connected != enabled {
        bail!(
            "OpenConnect did not {} within the expected time",
            if enabled { "connect" } else { "disconnect" }
        );
    }
    Ok(current)
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn openconnect_arguments(settings: &OpenConnectSettings) -> Result<Vec<String>> {
    let pid = pid_path()?.to_string_lossy().into_owned();
    let mut arguments = vec![
        format!("--protocol={}", settings.protocol),
        format!("--user={}", settings.username),
        "--passwd-on-stdin".into(),
        "--background".into(),
        format!("--pid-file={pid}"),
    ];
    if !settings.vpn_interface.trim().is_empty() {
        arguments.push(format!("--interface={}", settings.vpn_interface));
    }
    if !settings.auth_group.is_empty() {
        arguments.push(format!("--authgroup={}", settings.auth_group));
    }
    if !settings.vpnc_script.is_empty() {
        arguments.push(format!("--script={}", settings.vpnc_script));
    }
    arguments.extend(["--reconnect-timeout=1000".into(), settings.endpoint.clone()]);
    Ok(arguments)
}

#[cfg(target_os = "linux")]
fn start_openconnect(settings: &OpenConnectSettings, password: String) -> Result<()> {
    let pkexec = command_in_path("pkexec").ok_or_else(|| anyhow::anyhow!("pkexec is required to start OpenConnect"))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path()?)?;
    let stderr = log.try_clone()?;
    let mut child = std::process::Command::new(pkexec)
        .arg(&settings.executable)
        .args(openconnect_arguments(settings)?)
        .stdin(Stdio::piped())
        .stdout(log)
        .stderr(stderr)
        .spawn()
        .context("failed to request permission to start OpenConnect")?;
    let mut stdin = child
        .stdin
        .take()
        .context("failed to open OpenConnect standard input")?;
    stdin.write_all(password.as_bytes())?;
    stdin.write_all(b"\n")?;
    drop(stdin);
    let result = child.wait()?;
    if !result.success() {
        bail!("OpenConnect exited with status {result}");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn start_openconnect(settings: &OpenConnectSettings, password: String) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;

    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path()?)?;
    let secret_path = tunnel_dir()?.join(format!("openconnect-secret-{}", nanoid::nanoid!()));
    let mut secret = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&secret_path)?;
    secret.write_all(password.as_bytes())?;
    secret.write_all(b"\n")?;
    drop(secret);

    let executable = shell_single_quote(&settings.executable);
    let arguments = openconnect_arguments(settings)?
        .iter()
        .map(shell_single_quote)
        .collect::<Vec<_>>()
        .join(" ");
    let input = shell_single_quote(&secret_path);
    let log = shell_single_quote(log_path()?);
    let shell =
        format!("{executable} {arguments} < {input} >> {log} 2>&1; result=$?; /bin/rm -f {input}; exit $result");
    let script = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"Clash Verge needs permission to connect the campus VPN.\"",
        escape_osascript(&shell)
    );
    let result = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .status()
        .context("failed to request permission to start OpenConnect");
    let _ = std::fs::remove_file(&secret_path);
    let result = result?;
    if !result.success() {
        bail!("OpenConnect exited with status {result}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn stop_openconnect(pid: u32) -> Result<()> {
    let pkexec = command_in_path("pkexec").ok_or_else(|| anyhow::anyhow!("pkexec is required to stop OpenConnect"))?;
    let result = std::process::Command::new(pkexec)
        .arg(OsStr::new("/bin/kill"))
        .arg(OsStr::new("-TERM"))
        .arg(pid.to_string())
        .status()?;
    if !result.success() {
        bail!("Failed to stop OpenConnect: {result}");
    }
    remove_pid_file()
}

#[cfg(target_os = "macos")]
fn stop_openconnect(pid: u32) -> Result<()> {
    let shell = format!("/bin/kill -TERM {pid}");
    let script = format!(
        "do shell script \"{shell}\" with administrator privileges with prompt \"Clash Verge needs permission to disconnect the campus VPN.\""
    );
    let result = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .status()?;
    if !result.success() {
        bail!("Failed to stop OpenConnect: {result}");
    }
    remove_pid_file()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_pid_file() -> Result<()> {
    match std::fs::remove_file(pid_path()?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to remove the OpenConnect PID file"),
    }
}

#[cfg(target_os = "macos")]
fn shell_single_quote(value: impl AsRef<OsStr>) -> String {
    let value = value.as_ref().to_string_lossy();
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(target_os = "macos")]
fn escape_osascript(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "windows")]
fn wide(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(target_os = "windows")]
fn store_password(username: &str, password: &str) -> Result<()> {
    use std::ptr;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW,
    };

    let mut target = wide(CREDENTIAL_TARGET);
    let mut user = wide(username);
    let mut secret = password.as_bytes().to_vec();
    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        Comment: ptr::null_mut(),
        LastWritten: unsafe { std::mem::zeroed() },
        CredentialBlobSize: secret.len().try_into().context("password is too long")?,
        CredentialBlob: secret.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: ptr::null_mut(),
        TargetAlias: ptr::null_mut(),
        UserName: user.as_mut_ptr(),
    };
    let written = unsafe { CredWriteW(&raw const credential, 0) };
    secret.fill(0);
    if written == 0 {
        return Err(std::io::Error::last_os_error()).context("failed to store OpenConnect password");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn store_password(username: &str, password: &str) -> Result<()> {
    let _ = username;
    security_framework::passwords::set_generic_password(CREDENTIAL_TARGET, CREDENTIAL_ACCOUNT, password.as_bytes())
        .context("failed to store the OpenConnect password in Keychain")
}

#[cfg(target_os = "linux")]
fn store_password(username: &str, password: &str) -> Result<()> {
    let secret_tool = command_in_path("secret-tool")
        .ok_or_else(|| anyhow::anyhow!("secret-tool is required to store the OpenConnect password securely"))?;
    let label = format!("--label=Clash Verge OpenConnect ({username})");
    let mut child = std::process::Command::new(secret_tool)
        .args(["store", &label, "application", CREDENTIAL_TARGET])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("failed to start secret-tool")?;
    let mut stdin = child
        .stdin
        .take()
        .context("failed to open secret-tool standard input")?;
    stdin.write_all(password.as_bytes())?;
    drop(stdin);
    let result = child.wait()?;
    if !result.success() {
        bail!("secret-tool failed to store the OpenConnect password");
    }
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn store_password(_username: &str, _password: &str) -> Result<()> {
    bail!("Credential storage is not supported on this platform")
}

#[cfg(target_os = "windows")]
fn password_exists() -> Result<bool> {
    use std::ptr;
    use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW};

    let target = wide(CREDENTIAL_TARGET);
    let mut credential: *mut CREDENTIALW = ptr::null_mut();
    let read = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut credential) };
    if read == 0 {
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(1168) => Ok(false),
            _ => Err(std::io::Error::last_os_error()).context("failed to read OpenConnect credential"),
        };
    }
    unsafe { CredFree(credential.cast()) };
    Ok(true)
}

#[cfg(target_os = "macos")]
fn password_exists() -> Result<bool> {
    Ok(load_password()?.is_some())
}

#[cfg(target_os = "linux")]
fn password_exists() -> Result<bool> {
    if command_in_path("secret-tool").is_none() {
        return Ok(false);
    }
    Ok(load_password()?.is_some())
}

#[cfg(target_os = "macos")]
fn load_password() -> Result<Option<String>> {
    let options =
        security_framework::passwords::PasswordOptions::new_generic_password(CREDENTIAL_TARGET, CREDENTIAL_ACCOUNT);
    match security_framework::passwords::generic_password(options) {
        Ok(password) => String::from_utf8(password)
            .map(Some)
            .context("the OpenConnect password in Keychain is not valid UTF-8"),
        Err(error) if error.code() == -25300 => Ok(None),
        Err(error) => Err(error).context("failed to read the OpenConnect password from Keychain"),
    }
}

#[cfg(target_os = "linux")]
fn load_password() -> Result<Option<String>> {
    let secret_tool = command_in_path("secret-tool")
        .ok_or_else(|| anyhow::anyhow!("secret-tool is required to read the saved OpenConnect password"))?;
    let output = std::process::Command::new(secret_tool)
        .args(["lookup", "application", CREDENTIAL_TARGET])
        .output()
        .context("failed to start secret-tool")?;
    if !output.status.success() {
        return Ok(None);
    }
    let password =
        String::from_utf8(output.stdout).context("secret-tool returned a password that is not valid UTF-8")?;
    Ok(Some(password.trim_end_matches(['\r', '\n']).to_owned()))
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn password_exists() -> Result<bool> {
    Ok(false)
}

#[cfg(target_os = "windows")]
fn delete_password() -> Result<()> {
    use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};
    let target = wide(CREDENTIAL_TARGET);
    let deleted = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if deleted == 0 && std::io::Error::last_os_error().raw_os_error() != Some(1168) {
        return Err(std::io::Error::last_os_error()).context("failed to delete OpenConnect credential");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn delete_password() -> Result<()> {
    match security_framework::passwords::delete_generic_password(CREDENTIAL_TARGET, CREDENTIAL_ACCOUNT) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == -25300 => Ok(()),
        Err(error) => Err(error).context("failed to delete the OpenConnect password from Keychain"),
    }
}

#[cfg(target_os = "linux")]
fn delete_password() -> Result<()> {
    let Some(secret_tool) = command_in_path("secret-tool") else {
        return Ok(());
    };
    let result = std::process::Command::new(secret_tool)
        .args(["clear", "application", CREDENTIAL_TARGET])
        .status()?;
    if !result.success() {
        bail!("secret-tool failed to delete the OpenConnect password");
    }
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn delete_password() -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_routing_is_prepended_without_losing_profile_values() -> Result<()> {
        let config = serde_yaml_ng::from_str::<Mapping>(
            "tun:\n  route-exclude-address: [192.168.0.0/16]\ndns:\n  nameserver-policy: {}\nrules:\n  - MATCH,Proxy\n",
        )?;
        let settings = OpenConnectSettings {
            name: "Campus VPN".into(),
            executable: "openconnect.exe".into(),
            endpoint: "https://vpn.example.edu".into(),
            protocol: "anyconnect".into(),
            auth_group: String::new(),
            username: "student".into(),
            vpn_interface: "Campus VPN".into(),
            vpnc_script: String::new(),
            physical_interface: String::new(),
            route_prefixes: vec!["10.0.0.0/8".into()],
            direct_domains: vec!["example.edu".into()],
            dns_servers: vec!["10.0.0.53".into()],
        };

        let result = inject_split_routing(config, &settings);
        assert_eq!(
            result["tun"]["route-exclude-address"],
            Value::Sequence(vec![Value::from("10.0.0.0/8"), Value::from("192.168.0.0/16")])
        );
        assert_eq!(result["rules"][0], Value::from("DOMAIN-SUFFIX,example.edu,DIRECT"));
        assert_eq!(result["rules"][1], Value::from("IP-CIDR,10.0.0.0/8,DIRECT,no-resolve"));
        assert_eq!(result["rules"][2], Value::from("MATCH,Proxy"));
        assert_eq!(
            result["dns"]["nameserver-policy"]["+.example.edu"],
            Value::Sequence(vec![Value::from("10.0.0.53")])
        );
        Ok(())
    }

    #[test]
    fn unix_arguments_use_stdin_and_managed_pid_file() -> Result<()> {
        let settings = OpenConnectSettings {
            name: "Campus VPN".into(),
            executable: "/usr/bin/openconnect".into(),
            endpoint: "https://vpn.example.edu".into(),
            protocol: "anyconnect".into(),
            auth_group: "Students".into(),
            username: "student".into(),
            vpn_interface: "campus0".into(),
            vpnc_script: "/etc/vpnc/vpnc-script".into(),
            physical_interface: String::new(),
            route_prefixes: Vec::new(),
            direct_domains: Vec::new(),
            dns_servers: Vec::new(),
        };

        let arguments = openconnect_arguments(&settings)?;
        assert!(arguments.iter().any(|argument| argument == "--passwd-on-stdin"));
        assert!(arguments.iter().any(|argument| argument == "--background"));
        assert!(arguments.iter().any(|argument| argument.starts_with("--pid-file=")));
        assert!(arguments.iter().all(|argument| !argument.contains("password")));
        Ok(())
    }

    #[test]
    fn unix_arguments_allow_automatic_interface_selection() -> Result<()> {
        let settings = OpenConnectSettings {
            name: "Campus VPN".into(),
            executable: "/usr/bin/openconnect".into(),
            endpoint: "https://vpn.example.edu".into(),
            protocol: "anyconnect".into(),
            auth_group: String::new(),
            username: "student".into(),
            vpn_interface: String::new(),
            vpnc_script: String::new(),
            physical_interface: String::new(),
            route_prefixes: Vec::new(),
            direct_domains: Vec::new(),
            dns_servers: Vec::new(),
        };

        let arguments = openconnect_arguments(&settings)?;
        assert!(arguments.iter().all(|argument| !argument.starts_with("--interface=")));
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_helper_invocation_is_encoded_without_corrupting_spaced_paths() -> Result<()> {
        let command = openconnect_helper_command(
            Path::new(r"C:\Program Files\Clash Verge\resources\openconnect-helper.ps1"),
            "Connect",
            Path::new(r"C:\Users\Student Name\openconnect.json"),
            Path::new(r"C:\Users\Student Name\openconnect.pid"),
            Path::new(r"C:\Users\Student Name\openconnect.log"),
            "ClashVergeRev/OpenConnect/default",
        );

        assert!(command.contains(r"'C:\Program Files\Clash Verge\resources\openconnect-helper.ps1'"));
        assert!(!command.contains(r"C:\\Program Files"));

        let bytes = BASE64_STANDARD.decode(encode_powershell_command(&command))?;
        let decoded = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        assert_eq!(String::from_utf16(&decoded)?, command);
        Ok(())
    }
}

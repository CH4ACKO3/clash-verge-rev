use crate::utils::dirs;
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_yaml_ng::{Mapping, Value};
use std::ffi::OsString;
use std::net::IpAddr;
use std::path::PathBuf;
use tokio::fs;

const SETTINGS_FILE: &str = "openconnect.json";
const PID_FILE: &str = "openconnect.pid";
const LOG_FILE: &str = "openconnect.log";
const CREDENTIAL_TARGET: &str = "ClashVergeRev/OpenConnect/default";

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
        tokio::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .await
            .is_ok_and(|status| status.success())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = pid;
        false
    }
}

pub async fn set_connected(enabled: bool) -> Result<OpenConnectStatus> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = enabled;
        bail!("OpenConnect orchestration is currently supported on Windows only");
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
        let args = vec![
            OsString::from("-NoProfile"),
            OsString::from("-ExecutionPolicy"),
            OsString::from("Bypass"),
            OsString::from("-File"),
            helper.into_os_string(),
            OsString::from("-Action"),
            OsString::from(action),
            OsString::from("-SettingsPath"),
            settings_path()?.into_os_string(),
            OsString::from("-PidPath"),
            pid_path()?.into_os_string(),
            OsString::from("-LogPath"),
            log_path()?.into_os_string(),
            OsString::from("-CredentialTarget"),
            OsString::from(CREDENTIAL_TARGET),
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

#[cfg(not(target_os = "windows"))]
fn store_password(_username: &str, _password: &str) -> Result<()> {
    bail!("Credential storage is currently supported on Windows only")
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

#[cfg(not(target_os = "windows"))]
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

#[cfg(not(target_os = "windows"))]
fn delete_password() -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_routing_is_prepended_without_losing_profile_values() {
        let config = serde_yaml_ng::from_str::<Mapping>(
            "tun:\n  route-exclude-address: [192.168.0.0/16]\ndns:\n  nameserver-policy: {}\nrules:\n  - MATCH,Proxy\n",
        )
        .expect("valid fixture");
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
    }
}

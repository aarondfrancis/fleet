//! Parse and validate Runner Fleet inventories, preserving its format and rules.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use toml::Value;

use super::model::{Host, Inventory, LegacyRunner, Organization, Platform, Runner, ServiceMode, SshTransport};
use super::py::{PurePath, repr};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InventoryError(pub String);

type Result<T> = std::result::Result<T, InventoryError>;

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(InventoryError(message.into()))
}

/// Lets tests and other checkouts point the app at a different inventory.
pub const INVENTORY_PATH_ENV: &str = "RUNNER_DASHBOARD_INVENTORY";

/// The local, Git-ignored `inventory/fleet.toml`, unless overridden by the environment.
/// Create it from `inventory/fleet.example.toml` before using a live fleet.
pub fn default_inventory_path() -> PathBuf {
    if let Some(path) = std::env::var_os(INVENTORY_PATH_ENV) {
        return PathBuf::from(path);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../inventory/fleet.toml")
}

pub fn load_inventory(path: &Path, environ: &HashMap<String, String>) -> Result<Inventory> {
    let cannot_load =
        |detail: &dyn std::fmt::Display| InventoryError(format!("cannot load {}: {detail}", path.display()));
    let text = std::fs::read_to_string(path).map_err(|e| cannot_load(&e))?;
    let data = text.parse::<toml::Table>().map_err(|e| cannot_load(&e.message()))?;
    inventory_from_table(&data, environ)
}

/// Loads with the process environment, the way the app and CLI run.
pub fn load_inventory_from_env(path: &Path) -> Result<Inventory> {
    load_inventory(path, &std::env::vars().collect())
}

pub fn parse_inventory(text: &str, environ: &HashMap<String, String>) -> Result<Inventory> {
    let data = text
        .parse::<toml::Table>()
        .map_err(|e| InventoryError(format!("TOML: {}", e.message())))?;
    inventory_from_table(&data, environ)
}

fn inventory_from_table(data: &toml::Table, environ: &HashMap<String, String>) -> Result<Inventory> {
    let organization = match data.get("organization") {
        None => return err("invalid inventory structure: 'organization'"),
        Some(Value::Table(org)) => Organization {
            name: required_string(org, "name")?,
            url: required_string(org, "url")?,
            runner_group: required_string(org, "runner_group")?,
        },
        Some(_) => return err("organization must be a table"),
    };
    let hosts = match data.get("hosts") {
        None => Vec::new(),
        Some(Value::Array(raw)) => raw.iter().map(|h| parse_host(h, environ)).collect::<Result<Vec<_>>>()?,
        Some(_) => return err("hosts must be an array of tables"),
    };
    let inventory = Inventory { organization, hosts };
    validate_inventory(&inventory)?;
    Ok(inventory)
}

fn required_string(data: &toml::Table, key: &str) -> Result<String> {
    match data.get(key) {
        None => err(format!("invalid inventory structure: {}", repr(key))),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        Some(_) => err(format!("{key} must be a non-empty string")),
    }
}

fn strings(data: &toml::Table, key: &str) -> Result<Vec<String>> {
    match data.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.trim().to_string()),
                _ => err(format!("{key} must be an array of strings")),
            })
            .collect(),
        Some(_) => err(format!("{key} must be an array of strings")),
    }
}

fn resolve_ssh(value: &str, environ: &HashMap<String, String>) -> (Option<String>, String) {
    let setting = value.trim().to_string();
    if let Some(name) = env_reference(&setting) {
        let resolved = environ.get(name).map(|v| v.trim().to_string()).unwrap_or_default();
        return ((!resolved.is_empty()).then_some(resolved), setting);
    }
    (Some(setting.clone()), setting)
}

/// `${NAME}` where NAME is `[A-Z][A-Z0-9_]*`.
fn env_reference(setting: &str) -> Option<&str> {
    let name = setting.strip_prefix("${")?.strip_suffix('}')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    (first.is_ascii_uppercase() && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        .then_some(name)
}

fn optional_bool(data: &toml::Table, key: &str, default: bool, message: &str) -> Result<bool> {
    match data.get(key) {
        None => Ok(default),
        Some(Value::Boolean(b)) => Ok(*b),
        Some(_) => err(message),
    }
}

fn parse_host(raw: &Value, environ: &HashMap<String, String>) -> Result<Host> {
    let Value::Table(data) = raw else {
        return err("each host must be a table");
    };
    let (ssh_target, ssh_setting) = resolve_ssh(&required_string(data, "ssh")?, environ);
    let mut runners = Vec::new();
    match data.get("runners") {
        None => {}
        Some(Value::Array(raw_runners)) => {
            for raw in raw_runners {
                let Value::Table(raw) = raw else {
                    return err("each runner must be a table");
                };
                let replaces = match raw.get("replaces") {
                    None => None,
                    Some(Value::Table(replacement)) => {
                        let repository = match replacement.get("repository") {
                            None => None,
                            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
                            Some(_) => return err("legacy repository must be a non-empty string"),
                        };
                        Some(LegacyRunner {
                            name: required_string(replacement, "name")?,
                            directory: required_string(replacement, "directory")?,
                            repository,
                        })
                    }
                    Some(_) => return err("invalid inventory structure: 'replaces'"),
                };
                let decommission = optional_bool(raw, "decommission", false, "decommission must be true or false")?;
                runners.push(Runner {
                    name: required_string(raw, "name")?,
                    directory: required_string(raw, "directory")?,
                    labels: strings(raw, "labels")?,
                    exclude_labels: strings(raw, "exclude_labels")?,
                    replaces,
                    decommission,
                });
            }
        }
        Some(_) => return err("runners must be an array"),
    }
    let service_account = match data.get("service_account") {
        None => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(_) => return err("service_account must be a non-empty string"),
    };
    let enabled = optional_bool(data, "enabled", true, "enabled must be true or false")?;
    let id = required_string(data, "id")?;
    let platform = required_string(data, "platform")?;
    let root = required_string(data, "root")?;
    let service_mode = required_string(data, "service_mode")?;
    let labels = strings(data, "labels")?;
    let ssh_transport = match data.get("ssh_transport") {
        None => Some(SshTransport::Openssh),
        Some(Value::String(s)) => SshTransport::parse(s),
        Some(_) => None,
    }
    .ok_or_else(|| InventoryError(format!("{id}: ssh_transport must be openssh or tailscale")))?;
    let platform = Platform::parse(&platform)
        .ok_or_else(|| InventoryError(format!("{id}: unsupported platform {}", repr(&platform))))?;
    let service_mode = ServiceMode::parse(&service_mode).ok_or_else(|| service_mode_error(&id, platform))?;
    Ok(Host {
        id,
        platform,
        ssh_target,
        ssh_setting,
        root,
        service_mode,
        labels,
        runners,
        service_account,
        enabled,
        ssh_transport,
    })
}

fn service_mode_error(host_id: &str, platform: Platform) -> InventoryError {
    let choices: Vec<String> = platform.service_modes().iter().map(|m| repr(m.as_str())).collect();
    InventoryError(format!(
        "{host_id}: {platform} requires service_mode={}",
        choices.join(" or ")
    ))
}

/// A Windows account name of ASCII letters, digits, spaces and `.-_\@`, so it
/// can't break out of the quoting in the scripts it's written into.
fn safe_service_account(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_' | '\\' | '@'))
}

/// Host ids, runner names and exact runner paths already declared.
#[derive(Default)]
struct Declared<'a> {
    host_ids: HashSet<&'a str>,
    /// Inventory and legacy runner names share one namespace on GitHub.
    names: HashSet<&'a str>,
    /// Inventory and legacy runner directories, normalized, per host.
    paths: HashSet<(&'a str, String)>,
}

fn validate_inventory(inventory: &Inventory) -> Result<()> {
    let org = &inventory.organization;
    validate_name(&org.name, "organization name")?;
    let expected_url = format!("https://github.com/{}", org.name);
    if org.url.trim_end_matches('/') != expected_url {
        return err(format!("organization url must be {}", repr(&expected_url)));
    }
    if inventory.hosts.is_empty() {
        return err("inventory must define at least one host");
    }
    let mut declared = Declared::default();
    for host in &inventory.hosts {
        validate_host(host, &mut declared)?;
        for runner in &host.runners {
            validate_runner(host, runner, &mut declared)?;
            if let Some(legacy) = &runner.replaces {
                validate_legacy(host, legacy, org, &mut declared)?;
            }
        }
    }
    Ok(())
}

fn validate_host<'a>(host: &'a Host, declared: &mut Declared<'a>) -> Result<()> {
    validate_name(&host.id, "host id")?;
    if !declared.host_ids.insert(&host.id) {
        return err(format!("duplicate host id: {}", host.id));
    }
    if !host.platform.service_modes().contains(&host.service_mode) {
        return Err(service_mode_error(&host.id, host.platform));
    }
    if host.is_windows_logon() && host.service_account.is_none() {
        return err(format!(
            "{}: windows-logon requires service_account (the auto-logon user)",
            host.id
        ));
    }
    validate_root(host)?;
    if host.service_account.is_some() && !host.is_windows() {
        return err(format!("{}: service_account is supported only on Windows", host.id));
    }
    if let Some(account) = &host.service_account
        && !safe_service_account(account)
    {
        return err(format!("{}: invalid service_account: {}", host.id, repr(account)));
    }
    if host.runners.is_empty() {
        return err(format!("{}: define at least one runner", host.id));
    }
    for label in &host.labels {
        validate_name(label, &format!("{} label", host.id))?;
    }
    if !host.labels.iter().any(|l| l == "fleet") {
        return err(format!("{}: missing required labels: fleet", host.id));
    }
    let choices = host.platform.capability_labels();
    let present = choices.iter().filter(|c| host.labels.iter().any(|l| l == *c)).count();
    if present != 1 {
        return err(format!(
            "{}: include exactly one platform capability label: {}",
            host.id,
            choices.join(", ")
        ));
    }
    Ok(())
}

fn validate_runner<'a>(host: &'a Host, runner: &'a Runner, declared: &mut Declared<'a>) -> Result<()> {
    validate_name(&runner.name, "runner name")?;
    if runner.decommission && runner.replaces.is_some() {
        return err(format!(
            "{}: a decommissioned runner cannot declare replaces",
            runner.name
        ));
    }
    if !declared.names.insert(&runner.name) {
        return err(format!("duplicate runner name: {}", runner.name));
    }
    validate_relative_directory(host, &runner.directory)?;
    let normalized = normalized_path(host, &host.runner_path(runner));
    if !declared.paths.insert((&host.id, normalized.clone())) {
        return err(format!("duplicate runner path on {}: {normalized}", host.id));
    }
    for label in &runner.labels {
        validate_name(label, &format!("{} label", runner.name))?;
    }
    for label in &runner.exclude_labels {
        if !host.labels.contains(label) {
            return err(format!(
                "{}: exclude_labels may only remove host labels; {} is not one",
                runner.name,
                repr(label)
            ));
        }
        if label == "fleet" {
            return err(format!("{}: the fleet label cannot be excluded", runner.name));
        }
    }
    Ok(())
}

fn validate_legacy<'a>(
    host: &'a Host,
    legacy: &'a LegacyRunner,
    org: &Organization,
    declared: &mut Declared<'a>,
) -> Result<()> {
    validate_name(&legacy.name, "legacy runner name")?;
    if let Some(repository) = &legacy.repository {
        if !safe_repository(repository) {
            return err(format!("invalid legacy repository: {}", repr(repository)));
        }
        if !repository.starts_with(&format!("{}/", org.name)) {
            return err("legacy repository must belong to the inventory organization");
        }
    }
    if !declared.names.insert(&legacy.name) {
        return err(format!("duplicate runner name: {}", legacy.name));
    }
    validate_exact_legacy_path(host, &legacy.directory)?;
    if !declared
        .paths
        .insert((&host.id, normalized_path(host, &legacy.directory)))
    {
        return err(format!(
            "duplicate legacy runner path on {}: {}",
            host.id, legacy.directory
        ));
    }
    Ok(())
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]*$`
fn safe_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn safe_repository(value: &str) -> bool {
    match value.split_once('/') {
        Some((owner, repo)) => safe_name(owner) && safe_name(repo),
        None => false,
    }
}

fn validate_name(value: &str, description: &str) -> Result<()> {
    if safe_name(value) {
        Ok(())
    } else {
        err(format!("invalid {description}: {}", repr(value)))
    }
}

fn has_control(value: &str) -> bool {
    value.contains(['\n', '\r', '\t', '\0'])
}

fn validate_root(host: &Host) -> Result<()> {
    if has_control(&host.root) {
        return err(format!("{}: root contains a control character", host.id));
    }
    if host.is_windows() {
        let path = PurePath::windows(&host.root);
        let expected = PurePath::windows("C:/gh-runners").all_parts();
        let parts = path.all_parts();
        let beneath = parts.len() > expected.len()
            && parts
                .iter()
                .zip(&expected)
                .all(|(a, b)| a.to_lowercase() == b.to_lowercase());
        if !path.is_absolute() || !beneath {
            return err(format!("{}: Windows root must be beneath C:\\gh-runners", host.id));
        }
        for part in &parts[expected.len()..] {
            validate_name(part, &format!("{} root component", host.id))?;
        }
    } else {
        if !(host.root == "~/gh-runners" || host.root.starts_with("~/gh-runners/")) {
            return err(format!("{}: root must be beneath ~/gh-runners", host.id));
        }
        let parts = PurePath::posix(&host.root).all_parts();
        if parts.iter().any(|p| p == "..") {
            return err(format!("{}: root may not contain '..'", host.id));
        }
        for part in parts.iter().skip(2) {
            validate_name(part, &format!("{} root component", host.id))?;
        }
    }
    Ok(())
}

fn validate_relative_directory(host: &Host, directory: &str) -> Result<()> {
    let path = PurePath::new(directory, host.path_style());
    let parts = path.all_parts();
    if path.is_absolute() || parts.len() != 1 || parts[0] == "." || parts[0] == ".." {
        return err(format!(
            "{}: runner directory must be one safe path component: {}",
            host.id,
            repr(directory)
        ));
    }
    if has_control(directory) {
        return err(format!("{}: runner directory contains a control character", host.id));
    }
    validate_name(directory, &format!("{} runner directory", host.id))
}

fn validate_exact_legacy_path(host: &Host, directory: &str) -> Result<()> {
    if has_control(directory) {
        return err(format!("{}: legacy directory contains a control character", host.id));
    }
    if host.is_windows() {
        let path = PurePath::windows(directory);
        if !path.is_absolute() {
            return err(format!("{}: legacy Windows path must be absolute", host.id));
        }
        if path.all_parts().len() < 2 {
            return err(format!("{}: legacy Windows path is too broad", host.id));
        }
    } else if !(directory.starts_with("~/") || directory.starts_with('/')) {
        return err(format!("{}: legacy path must be an exact absolute or ~/ path", host.id));
    } else if directory == "/" || directory == "~/" {
        return err(format!("{}: legacy path is too broad", host.id));
    }
    if PurePath::new(directory, host.path_style())
        .all_parts()
        .iter()
        .any(|p| p == "..")
    {
        return err(format!("{}: legacy path may not contain '..'", host.id));
    }
    Ok(())
}

fn normalized_path(host: &Host, value: &str) -> String {
    let result = PurePath::new(value, host.path_style()).to_string();
    if host.is_windows() {
        result.to_lowercase()
    } else {
        result
    }
}

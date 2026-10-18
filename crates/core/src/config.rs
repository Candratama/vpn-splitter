use crate::Error;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fmt,
    fs::{DirBuilder, File},
    io::{Read, Write},
    net::Ipv4Addr,
    path::Path,
};

pub const MAX_DESTINATIONS: usize = 4_096;
pub const MAX_CONFIG_BYTES: usize = 1_048_576;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Destination {
    Ipv4(Ipv4Addr),
    Cidr { network: Ipv4Addr, prefix: u8 },
    Hostname(String),
}

impl TryFrom<String> for Destination {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        let value = value.trim();
        if let Some((address, prefix)) = value.split_once('/') {
            let address = address
                .parse::<Ipv4Addr>()
                .map_err(|_| invalid("Invalid IPv4 network"))?;
            if prefix.is_empty() || prefix.len() > 2 || !prefix.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(invalid("Invalid IPv4 prefix"));
            }
            let prefix = prefix
                .parse::<u8>()
                .map_err(|_| invalid("Invalid IPv4 prefix"))?;
            if !(2..=32).contains(&prefix) {
                return Err(invalid("IPv4 prefixes must be between 2 and 32"));
            }
            let network = Ipv4Addr::from(u32::from(address) & (u32::MAX << (32 - prefix)));
            return Ok(Self::Cidr { network, prefix });
        }
        if let Ok(address) = value.parse::<Ipv4Addr>() {
            return Ok(Self::Ipv4(address));
        }
        let host = value
            .strip_suffix('.')
            .unwrap_or(value)
            .to_ascii_lowercase();
        if host.is_empty()
            || host.len() > 253
            || !host.bytes().any(|b| b.is_ascii_alphabetic())
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.as_bytes()[0].is_ascii_alphanumeric()
                    && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err(invalid(
                "Expected IPv4, CIDR, or an explicit ASCII hostname (no URL, wildcard, or IPv6)",
            ));
        }
        Ok(Self::Hostname(host))
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ipv4(ip) => write!(f, "{ip}"),
            Self::Cidr { network, prefix } => write!(f, "{network}/{prefix}"),
            Self::Hostname(host) => f.write_str(host),
        }
    }
}

impl From<Destination> for String {
    fn from(value: Destination) -> Self {
        value.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub destinations: Vec<Destination>,
    #[serde(default)]
    pub launch_at_login: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            profile_id: None,
            destinations: Vec::new(),
            launch_at_login: false,
        }
    }
}

impl Config {
    pub fn parse(input: &str) -> Result<Self, Error> {
        if input.len() > MAX_CONFIG_BYTES {
            return Err(invalid("Config exceeds 1 MiB"));
        }
        let config: Self = serde_json::from_str(input).map_err(|error| {
            invalid(&format!(
                "Invalid config JSON at line {}, column {}",
                error.line(),
                error.column()
            ))
        })?;
        config.normalized()
    }

    pub fn normalized(&self) -> Result<Self, Error> {
        if self.version != 1 {
            return Err(invalid("Unsupported config version"));
        }
        if let Some(id) = &self.profile_id
            && (id.trim().is_empty() || id.len() > 512 || id.chars().any(char::is_control))
        {
            return Err(invalid("Invalid saved VPN profile reference"));
        }
        Ok(Self {
            destinations: normalize(self.destinations.iter().map(ToString::to_string))?,
            ..self.clone()
        })
    }
}

fn invalid(message: &str) -> Error {
    Error::InvalidInput(message.into())
}

fn normalize(entries: impl IntoIterator<Item = String>) -> Result<Vec<Destination>, Error> {
    let mut seen = HashSet::new();
    let mut destinations = Vec::new();
    for entry in entries {
        let destination = Destination::try_from(entry)?;
        if seen.insert(destination.clone()) {
            destinations.push(destination);
        }
        if destinations.len() > MAX_DESTINATIONS {
            return Err(invalid("Too many destinations (maximum 4096)"));
        }
    }
    Ok(destinations)
}

pub fn import_rules(input: &str) -> Result<Vec<Destination>, Error> {
    if input.len() > MAX_CONFIG_BYTES {
        return Err(invalid("Import exceeds 1 MiB"));
    }
    normalize(input.lines().filter_map(|line| {
        let value = line.split('#').next().unwrap_or_default().trim();
        (!value.is_empty()).then(|| value.to_owned())
    }))
}

pub fn load_config(path: &Path) -> Result<Config, Error> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => return Err(error.into()),
    };
    let mut input = String::new();
    file.take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_string(&mut input)?;
    Config::parse(&input)
}

pub fn save_config(path: &Path, config: &Config) -> Result<(), Error> {
    let config = config.normalized()?;
    let bytes = serde_json::to_vec_pretty(&config).map_err(|_| invalid("Cannot encode config"))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(invalid("Config exceeds 1 MiB"));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent)?;
    #[cfg(unix)]
    let directory = File::open(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".vpn-splitter-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| Error::Io(error.error))?;
    #[cfg(unix)]
    directory.sync_all().map_err(|error| {
        Error::Io(std::io::Error::new(
            error.kind(),
            "Config was replaced but directory sync failed; reload before retrying",
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_import_normalizes_and_deduplicates_without_reordering() {
        let rules = import_rules("# office\n198.51.100.129/24 # network\nGit.Office.Example.\ngit.office.example\n203.0.113.7\n").unwrap();
        assert_eq!(
            rules,
            vec![
                Destination::Cidr {
                    network: Ipv4Addr::new(198, 51, 100, 0),
                    prefix: 24
                },
                Destination::Hostname("git.office.example".into()),
                Destination::Ipv4(Ipv4Addr::new(203, 0, 113, 7)),
            ]
        );
    }

    #[test]
    fn config_import_rejects_invalid_or_overbroad_destinations() {
        for input in [
            "999.1.2.3",
            "198.51.100.1/33",
            "198.51.100.1/-1",
            "198.51.100.1/24/2",
            "0.0.0.0/0",
            "128.0.0.0/1",
            "https://git.office.example",
            "*.office.example",
            "2001:db8::1",
            "-bad.office.example",
            "bad_.office.example",
            "a..office.example",
            "git office.example",
        ] {
            assert!(import_rules(input).is_err(), "must reject {input}");
        }
    }

    #[test]
    fn config_limits_unique_destinations() {
        let input = (0..4097)
            .map(|i| format!("host{i}.office.example"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(import_rules(&input).is_err());
        assert_eq!(
            import_rules(&vec!["git.office.example"; 4097].join("\n"))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn config_json_rejects_unknown_versions_fields_and_bad_profile_ids() {
        for input in [
            r#"{"version":2}"#,
            r#"{"version":1,"password":"example-not-a-secret"}"#,
            r#"{"version":1,"profile_id":""}"#,
            r#"{"version":1,"profile_id":"bad\u0000id"}"#,
        ] {
            assert!(Config::parse(input).is_err());
        }
        let c = Config::parse(
            r#"{"version":1,"destinations":["Git.Office.Example","git.office.example"]}"#,
        )
        .unwrap();
        assert_eq!(
            c.destinations,
            vec![Destination::Hostname("git.office.example".into())]
        );
    }

    #[test]
    fn config_missing_file_defaults_but_corruption_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(load_config(&path).unwrap(), Config::default());
        std::fs::write(&path, "{broken").unwrap();
        assert!(load_config(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{broken");
    }

    #[test]
    fn config_replacement_is_private_and_invalid_data_leaves_previous_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private/config.json");
        let mut config = Config {
            destinations: import_rules("198.51.100.7\ngit.office.example").unwrap(),
            ..Config::default()
        };
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
        let before = std::fs::read(&path).unwrap();
        let invalid = Config {
            version: 2,
            ..config.clone()
        };
        assert!(save_config(&path, &invalid).is_err());
        let invalid = Config {
            destinations: vec![Destination::Hostname("*.office.example".into())],
            ..config.clone()
        };
        assert!(save_config(&path, &invalid).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        config.launch_at_login = true;
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap(), config);
    }

    #[test]
    fn config_rejects_oversized_input_without_partial_save() {
        assert!(Config::parse(&" ".repeat(1_048_577)).is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, vec![b' '; 1_048_577]).unwrap();
        assert!(load_config(&path).is_err());
        assert_eq!(std::fs::metadata(path).unwrap().len(), 1_048_577);
    }

    #[test]
    fn config_writer_crash_leaves_old_or_new_complete_json() {
        use std::{
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let old = Config::default();
        save_config(&path, &old).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "config::tests::config_atomic_writer_child",
            ])
            .env("VPN_SPLITTER_TEST_CONFIG", &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let staged = loop {
            if std::fs::read_dir(dir.path()).unwrap().any(|e| {
                e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".vpn-splitter-")
            }) {
                break true;
            }
            if Instant::now() > until {
                break false;
            }
            std::thread::yield_now();
        };
        let _ = child.kill();
        child.wait().unwrap();
        assert!(
            staged,
            "writer must reach atomic staging before termination"
        );
        let actual = load_config(&path).unwrap();
        assert!(actual == old || actual == large_config());
    }

    fn large_config() -> Config {
        let label = "a".repeat(60);
        Config {
            destinations: (0..4096)
                .map(|i| Destination::Hostname(format!("host{i}.{label}.{label}.office.example")))
                .collect(),
            ..Config::default()
        }
    }

    #[test]
    #[ignore = "subprocess fixture used by config_writer_crash_leaves_old_or_new_complete_json"]
    fn config_atomic_writer_child() {
        let path = std::env::var_os("VPN_SPLITTER_TEST_CONFIG").expect("test fixture path");
        loop {
            save_config(Path::new(&path), &large_config()).unwrap();
        }
    }
}

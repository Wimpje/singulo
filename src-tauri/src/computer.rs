use crate::model::{Computer, Result};
use std::{process::Command, sync::OnceLock};

fn output(mut command: Command) -> Result<String> {
    let result = command.output().map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(format!("Computer identification failed: {}", result.status));
    }
    String::from_utf8(result.stdout).map_err(|e| e.to_string())
}

fn identify() -> Result<Computer> {
    #[cfg(target_os = "macos")]
    let (name, machine_id) = {
        let name = output(Command::new("/bin/hostname"))?;
        let mut command = Command::new("/usr/sbin/ioreg");
        command.args(["-rd1", "-c", "IOPlatformExpertDevice"]);
        let data = output(command)?;
        let id = data
            .lines()
            .find(|line| line.contains("\"IOPlatformUUID\""))
            .and_then(|line| line.split('"').nth(3))
            .ok_or("Could not read the computer identifier.")?;
        (name, id.to_string())
    };
    #[cfg(windows)]
    let (name, machine_id) = {
        let system =
            std::env::var_os("SystemRoot").ok_or("Windows system directory is missing.")?;
        let system = std::path::PathBuf::from(system).join("System32");
        let name = std::env::var("COMPUTERNAME")
            .map_err(|e| format!("Cannot read the Windows computer name: {e}"))?;
        let mut command = Command::new(system.join("reg.exe"));
        command.args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ]);
        let data = output(command)?;
        let id = data
            .lines()
            .find(|line| line.contains("MachineGuid"))
            .and_then(|line| line.split_whitespace().last())
            .ok_or("Could not read the computer identifier.")?;
        (name, id.to_string())
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let (name, machine_id) = (
        output(Command::new("hostname"))?,
        std::fs::read_to_string("/etc/machine-id").map_err(|e| e.to_string())?,
    );
    let name = name.trim();
    let machine_id = machine_id.trim();
    if name.is_empty() || machine_id.is_empty() {
        return Err("The computer name or identifier is empty.".into());
    }
    let key = format!("{}:{machine_id}:{name}", std::env::consts::OS);
    Ok(Computer {
        id: blake3::hash(key.as_bytes()).to_hex().to_string(),
        name: name.into(),
    })
}

pub fn current() -> Result<Computer> {
    static COMPUTER: OnceLock<Result<Computer>> = OnceLock::new();
    COMPUTER.get_or_init(identify).clone()
}

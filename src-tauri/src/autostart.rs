//! Starting Envelope when the user logs in (an XDG autostart entry). Inside
//! Flatpak the desktop's Background portal writes the entry for us.

use std::path::PathBuf;

const ENTRY: &str = "io.github.burakgon.Envelope.desktop";

fn entry_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("autostart").join(ENTRY))
}

/// How to launch this copy of Envelope: the AppImage file, the Flatpak, or
/// the running executable.
fn launch_command() -> String {
    if let Ok(id) = std::env::var("FLATPAK_ID") {
        return format!("flatpak run {id}");
    }
    let exe = std::env::var("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|_| std::env::current_exe())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "envelope".into());
    if exe.contains([' ', '"', '\\']) {
        format!("\"{}\"", exe.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        exe
    }
}

pub fn is_enabled() -> bool {
    entry_path().is_some_and(|p| p.exists())
}

pub async fn set(enabled: bool) -> Result<(), String> {
    // The portal's entry lives outside the sandbox, where it cannot be read
    // back; the copy written below into the app's own configuration
    // directory remembers the answer.
    #[cfg(target_os = "linux")]
    if std::env::var_os("FLATPAK_ID").is_some() {
        portal(enabled).await?;
    }
    write_entry(enabled)
}

#[cfg(target_os = "linux")]
async fn portal(enabled: bool) -> Result<(), String> {
    use ashpd::desktop::ResponseError;
    use ashpd::desktop::background::Background;

    let response = match Background::request()
        .reason("Check for new mail and reminders after you log in")
        .auto_start(enabled)
        .command(["envelope", "--minimized"])
        .send()
        .await
        .and_then(|r| r.response())
    {
        Ok(r) => r,
        Err(ashpd::Error::Response(ResponseError::Cancelled)) if enabled => {
            return Err("Starting at login was not allowed.".into());
        }
        Err(e) => return Err(format!("The desktop did not accept the request: {e}")),
    };
    if enabled && !response.auto_start() {
        return Err(
            "Starting at login was not allowed. You can change this in your system settings."
                .into(),
        );
    }
    Ok(())
}

fn write_entry(enabled: bool) -> Result<(), String> {
    let path = entry_path().ok_or("cannot find the configuration directory")?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Envelope\n\
         Comment=Mail and calendar\n\
         Icon=io.github.burakgon.Envelope\n\
         Exec={} --minimized\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        launch_command()
    );
    std::fs::write(&path, entry).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_removes_the_entry() {
        let dir = std::env::temp_dir().join(format!("envelope-autostart-{}", std::process::id()));
        // SAFETY: this test is the only code in the crate's tests that reads these variables.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &dir);
            std::env::set_var("APPIMAGE", "/home/me/Apps/Envelope 0.1.0.AppImage");
            std::env::remove_var("FLATPAK_ID");
        }
        assert!(!is_enabled());
        write_entry(true).unwrap();
        assert!(is_enabled());
        let entry = std::fs::read_to_string(dir.join("autostart").join(ENTRY)).unwrap();
        assert!(
            entry.contains("Exec=\"/home/me/Apps/Envelope 0.1.0.AppImage\" --minimized"),
            "{entry}"
        );
        write_entry(false).unwrap();
        assert!(!is_enabled());
        write_entry(false).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

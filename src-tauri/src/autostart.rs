//! Starting Envelope when the user logs in (an XDG autostart entry).

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

pub fn set(enabled: bool) -> Result<(), String> {
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
        set(true).unwrap();
        assert!(is_enabled());
        let entry = std::fs::read_to_string(dir.join("autostart").join(ENTRY)).unwrap();
        assert!(
            entry.contains("Exec=\"/home/me/Apps/Envelope 0.1.0.AppImage\" --minimized"),
            "{entry}"
        );
        set(false).unwrap();
        assert!(!is_enabled());
        set(false).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

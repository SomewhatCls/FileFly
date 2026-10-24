//! Registers FileFly as the default handler for folders (and restores Windows Explorer).
//!
//! Everything is written to HKEY_CURRENT_USER\Software\Classes, which overrides the machine-wide
//! defaults in HKLM, needs no admin rights and can be removed again without touching the system.
//! The keys FileFly creates carry a `FileFlyManaged` marker value, so "restore" only ever removes
//! what FileFly itself wrote.

#[cfg(windows)]
mod imp {
    use std::io;
    use winreg::{enums::*, RegKey};

    /// Classes whose `open` verb is redirected: normal folders, the general folder class, drive roots.
    const CLASSES: [&str; 2] = ["Directory", "Drive"];
    const LEGACY_CLASSES: [&str; 1] = ["Folder"];
    const MARKER: &str = "FileFlyManaged";

    fn shell_path(class: &str) -> String {
        format!(r"Software\Classes\{class}\shell")
    }

    /// `"C:\...\FileFly.exe" --open "%1"` for the running executable.
    fn command() -> io::Result<String> {
        let exe = std::env::current_exe()?;
        Ok(format!("\"{}\" --open \"%1\"", exe.display()))
    }

    /// Tells the shell that file associations changed so the new handler is picked up right away.
    fn notify_shell() {
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn SHChangeNotify(
                event: i32,
                flags: u32,
                item1: *const core::ffi::c_void,
                item2: *const core::ffi::c_void,
            );
        }
        const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, 0, std::ptr::null(), std::ptr::null()) };
    }

    fn remove_class(hkcu: &RegKey, class: &str) -> io::Result<()> {
        let shell = shell_path(class);
        let open_path = format!(r"{shell}\open");

        // Only remove what FileFly wrote.
        let ours = hkcu
            .open_subkey(&open_path)
            .map(|k| k.get_value::<u32, _>(MARKER).is_ok())
            .unwrap_or(false);
        if !ours {
            return Ok(());
        }
        hkcu.delete_subkey_all(&open_path)?;

        if let Ok(key) = hkcu.open_subkey_with_flags(&shell, KEY_READ | KEY_WRITE) {
            let is_open = key
                .get_value::<String, _>("")
                .map_or(false, |v| v.eq_ignore_ascii_case("open"));
            if is_open {
                let _ = key.delete_value("");
            }
        }
        // Removes the `shell` key only if nothing else lives in it.
        let _ = hkcu.delete_subkey(&shell);
        Ok(())
    }

    /// True when the keys written by FileFly exist and point at *this* executable.
    pub fn is_default() -> bool {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let Ok(open) = hkcu.open_subkey(format!(r"{}\open", shell_path("Directory"))) else {
            return false;
        };
        if open.get_value::<u32, _>(MARKER).is_err() {
            return false;
        }
        let stored: String = open
            .open_subkey("command")
            .and_then(|k| k.get_value(""))
            .unwrap_or_default();
        command().map_or(false, |c| c.eq_ignore_ascii_case(&stored))
    }

    pub fn set_default() -> io::Result<()> {
        let cmd = command()?;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // Undo what older versions wrote, so existing users get fixed on the next apply.
        for class in LEGACY_CLASSES {
            remove_class(&hkcu, class)?;
        }

        for class in CLASSES {
            let shell = shell_path(class);

            let (shell_key, _) = hkcu.create_subkey(&shell)?;
            shell_key.set_value("", &"open")?;

            let (open, _) = hkcu.create_subkey(format!(r"{shell}\open"))?;
            open.set_value(MARKER, &1u32)?;

            let (command_key, _) = hkcu.create_subkey(format!(r"{shell}\open\command"))?;
            command_key.set_value("", &cmd)?;
        }
        notify_shell();
        Ok(())
    }

    pub fn restore_default() -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for class in CLASSES.iter().chain(LEGACY_CLASSES.iter()) {
            remove_class(&hkcu, class)?;
        }
        notify_shell();
        Ok(())
    }
}

#[cfg(windows)]
pub use imp::{is_default, restore_default, set_default};

#[cfg(not(windows))]
pub fn is_default() -> bool {
    false
}
#[cfg(not(windows))]
pub fn set_default() -> std::io::Result<()> {
    Err(std::io::Error::other("Only available on Windows"))
}
#[cfg(not(windows))]
pub fn restore_default() -> std::io::Result<()> {
    Ok(())
}

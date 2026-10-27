//! Windows toast notifications, the fallback while the notch hides behind a fullscreen
//! app.

use std::ffi::c_void;
use std::iter;

use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
use windows::core::{HSTRING, w};

/// The identity toasts are shown under, registered by [`register`].
const APP_ID: &str = "notchling";

/// Shows `text` as a toast from notchling.
///
/// # Errors
///
/// Fails when the app identity cannot be registered or Windows refuses the toast.
pub fn show(text: &str) -> anyhow::Result<()> {
    register()?;
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(toast_xml(text)))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)?;
    Ok(())
}

/// Registers the app identity under `HKCU\Software\Classes\AppUserModelId`. Windows
/// only shows toasts from apps it knows, and an app without an installer has no Start
/// menu shortcut to carry an identity. Writing the same value again is harmless.
fn register() -> anyhow::Result<()> {
    let name: Vec<u16> = APP_ID.encode_utf16().chain(iter::once(0)).collect();
    let bytes = u32::try_from(name.len() * 2).expect("the app name is short");
    // SAFETY: `name` is a NUL-terminated UTF-16 string of `bytes` bytes that outlives
    // the call; the key and value names are static wide strings.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Classes\AppUserModelId\notchling"),
            w!("DisplayName"),
            REG_SZ.0,
            Some(name.as_ptr().cast::<c_void>()),
            bytes,
        )
    }
    .ok()?;
    Ok(())
}

/// A plain toast with `text` as its only line.
fn toast_xml(text: &str) -> String {
    format!(
        r#"<toast><visual><binding template="ToastGeneric"><text>{}</text></binding></visual></toast>"#,
        escape_xml(text)
    )
}

/// Escapes the characters XML treats specially, so project names cannot break the toast.
fn escape_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_characters_are_escaped() {
        assert_eq!(
            escape_xml(r#"Done · R&D <"x"> 'y'"#),
            "Done · R&amp;D &lt;&quot;x&quot;&gt; &apos;y&apos;"
        );
        assert!(toast_xml("a & b").contains("<text>a &amp; b</text>"));
    }
}

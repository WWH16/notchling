#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[allow(missing_debug_implementations, reason = "slint-generated types")]
mod ui {
    slint::include_modules!();
}

use slint::ComponentHandle;
use ui::Notch;

fn main() -> anyhow::Result<()> {
    let notch = Notch::new()?;
    notch.run()?;
    Ok(())
}

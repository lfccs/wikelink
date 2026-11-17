//! Colours shared by the settings window and the in-game overlay (the site's RSI palette).

use eframe::egui::Color32;

pub const ACCENT: Color32 = Color32::from_rgb(0x3a, 0xc0, 0xe6);
pub const OK: Color32 = Color32::from_rgb(0x4c, 0xd1, 0x7a);
pub const WARN: Color32 = Color32::from_rgb(0xe8, 0xb0, 0x4a);
pub const BAD: Color32 = Color32::from_rgb(0xe8, 0x6a, 0x6a);
pub const MUTED: Color32 = Color32::from_rgb(0x8a, 0x97, 0xa6);
/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(0xd6, 0xde, 0xe8);
/// The app's navy: panel background.
pub const NAVY: Color32 = Color32::from_rgb(0x0b, 0x12, 0x1a);
/// Darker than the panel: text fields, the log box.
pub const NAVY_DEEP: Color32 = Color32::from_rgb(0x07, 0x0c, 0x12);
/// Group boxes and inactive widgets.
pub const NAVY_RAISED: Color32 = Color32::from_rgb(0x0e, 0x17, 0x21);
pub const BORDER: Color32 = Color32::from_rgb(0x1f, 0x2f, 0x40);
/// The brand brass (`--brand` on the site): the overlay marks what is stored right here.
pub const BRASS: Color32 = Color32::from_rgb(0xe6, 0xa9, 0x4a);
/// The overlay's search field: clearly lighter than the panel around it, so it reads as a
/// field over a dark game scene.
pub const FIELD: Color32 = Color32::from_rgb(0x1d, 0x2d, 0x3e);
/// Its border while it does not have the keyboard (with it: [`ACCENT`]).
pub const FIELD_BORDER: Color32 = Color32::from_rgb(0x5a, 0x70, 0x88);
/// Its hint text: readable on [`FIELD`].
pub const FIELD_HINT: Color32 = Color32::from_rgb(0xa8, 0xb5, 0xc4);
/// Typed text, brighter than [`TEXT`].
pub const TEXT_BRIGHT: Color32 = Color32::from_rgb(0xf2, 0xf6, 0xfa);

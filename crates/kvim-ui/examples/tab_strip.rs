//! Cycles one host through its named surfaces with one pair of keys.
//!
//! Run it with `cargo run -p kvim-ui --example tab_strip`.
//!
//! A host that shows a chat, an editor, and a review needs one mapping for each
//! surface, or one strip that it walks. The strip holds the order, the labels,
//! and the active surface. It owns no surface value: the host reads the active
//! identity and draws that surface itself.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use kvim_ui::TabStrip;

/// The surfaces that this host owns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Surface {
    Chat,
    Editor,
    Review,
}

impl Surface {
    /// Returns the label that the strip draws for the surface.
    const fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Editor => "Editor",
            Self::Review => "Review",
        }
    }
}

/// The strip band of this host.
const STRIP: Rect = Rect {
    x: 0,
    y: 0,
    width: 40,
    height: 1,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut tabs = TabStrip::default();
    for surface in [Surface::Chat, Surface::Editor, Surface::Review] {
        tabs.open(surface, surface.label())?;
    }

    // The first tab owns the strip, so a host always holds one active surface.
    println!("the strip opens on {:?}", tabs.active());

    // One key walks every surface, so the host needs no mapping for each one.
    for _ in 0..4 {
        tabs.select_next();
        println!("the next key reaches {:?}", tabs.active());
    }

    // A host answers a mouse click with the same places that it draws.
    tabs.select(&Surface::Editor);
    let mut cells = Buffer::empty(STRIP);
    tabs.render(&mut cells, STRIP, |target, placement| {
        let style = if placement.tab.active {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        target.set_string(
            placement.area.x,
            placement.area.y,
            format!(" {} ", placement.tab.label),
            style,
        );
    });

    let drawn: String = (0..STRIP.width)
        .map(|x| cells[(x, 0)].symbol().chars().next().unwrap_or(' '))
        .collect();
    println!("the strip draws {:?}", drawn.trim_end());

    for placement in tabs.placements(STRIP) {
        println!(
            "  {} sits at {:?}, active {}",
            placement.tab.label, placement.area, placement.tab.active
        );
    }

    // Closing one surface closes its tab, and the strip keeps one active tab.
    tabs.close(&Surface::Chat);
    println!("after one close the strip holds {tabs}");
    Ok(())
}

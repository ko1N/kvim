//! Sheds the parts of one host-owned band in the order that the host stated.
//!
//! Run it with `cargo run -p kvim-ui --example chrome_band`.
//!
//! A host that draws its own statusline names its own subjects. This host names
//! a connection state, a room, and an unread count. The band names none of them:
//! it takes the text that the host rendered, the edge that each part sits
//! against, and one rank for each part. A band that cannot hold every part sheds
//! the lowest rank first, and the highest rank survives every shed.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use kvim_ui::{BandRank, BandSegment, ChromeBand};

/// The widths that this host draws its band at.
const WIDTHS: [u16; 4] = [40, 20, 10, 4];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The host ranks its own parts. The connection state survives longest,
    // because it tells the reader whether anything else is still true.
    let band = ChromeBand::new(vec![
        BandSegment::left(" ONLINE ", BandRank::new(2)),
        BandSegment::right("#general ", BandRank::new(1)),
        BandSegment::right("3 unread ", BandRank::new(0)),
    ])?;

    for width in WIDTHS {
        let area = Rect::new(0, 0, width, 1);
        let mut cells = Buffer::empty(area);
        for placement in band.placements(area) {
            // The band answers the place. The host owns every color, so it
            // draws the state of the connection in its own emphasis.
            let style = match placement.segment.rank.get() {
                2 => Style::default().add_modifier(Modifier::REVERSED),
                _ => Style::default(),
            };
            cells.set_string(
                placement.area.x,
                placement.area.y,
                placement.segment.text,
                style,
            );
        }

        let drawn: String = (0..width)
            .map(|x| cells[(x, 0)].symbol().chars().next().unwrap_or(' '))
            .collect();
        println!("{width:>3} cells draw {drawn:?}");
        for placement in band.placements(area) {
            println!(
                "      {:?} sits at {:?}",
                placement.segment.text, placement.area
            );
        }
    }
    Ok(())
}

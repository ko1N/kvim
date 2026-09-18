//! Paint one caller-owned edited line and read its terminal cursor position.
//!
//! Run it with `cargo run -p kvim-ui --example line_input`.

use kvim_ui::{LineInput, LineInputStyles};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let area = Rect::new(0, 0, 12, 1);
    let mut target = Buffer::empty(area);
    let text = "edit 語 here";
    let cursor = LineInput::render(
        &mut target,
        area,
        "> ",
        text,
        text.len(),
        LineInputStyles {
            field: Style::default(),
            prefix: Style::default().fg(Color::Yellow),
            text: Style::default().fg(Color::White),
        },
    )?;

    let rendered: String = (area.x..area.right())
        .map(|x| {
            target
                .cell((x, area.y))
                .expect("the example reads inside its buffer")
                .symbol()
        })
        .collect();
    println!("{rendered}");
    println!("cursor: {}, {}", cursor.x, cursor.y);
    Ok(())
}

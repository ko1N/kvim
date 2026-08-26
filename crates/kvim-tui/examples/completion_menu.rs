//! Open, cycle, draw, and cancel one candidate menu over host-owned candidates.
//!
//! The example is one complete host of the candidate menu that kvim publishes.
//! It holds a prompt line of its own, offers candidates of its own vocabulary,
//! and paints them with the appearance of kvim.
//!
//! The run proves five facts of `docs/embedding.md`:
//!
//! - the host opens one menu over its own candidates and reads the selection;
//! - the cycle wraps at both ends, so every candidate is reachable;
//! - a cancelled menu restores the text that the reader typed, exactly;
//! - the entry of a row names the candidate alone. The host paints its own `:`
//!   prefix in front of its prompt line, and the menu repeats no prefix;
//! - the bound refuses a longer candidate list instead of cutting it, so no
//!   candidate ever disappears without the host learning of it.
//!
//! kvim's own command line paints through the same `draw_completion_menu` call,
//! so the host and the standalone editor cannot show two appearances.
//!
//! The example needs no editor, no filesystem, and no terminal.
//!
//! Run it with:
//!
//! ```sh
//! cargo run -p kvim-tui --example completion_menu
//! ```

use ratatui::buffer::Buffer as CellBuffer;
use ratatui::layout::Rect;

use kvim_tui::{
    COMPLETION_CANDIDATES_MAX, CompletionCycle, CompletionOutcome, LineCompletion, Theme,
    draw_completion_menu,
};

/// The band that the host gives the menu, in terminal cells.
///
/// The menu takes the last rows of this rectangle, so the prompt line of the
/// host stays visible below it.
const BODY: Rect = Rect {
    x: 0,
    y: 0,
    width: 24,
    height: 6,
};

/// The largest number of characters that the prompt line of the host accepts.
const PROMPT_CHARS_MAX: usize = 64;

/// The prefix that the host paints in front of its own prompt line.
///
/// The prefix belongs to the prompt line and never to one menu entry.
const PROMPT_PREFIX: &str = ":";

fn main() {
    let typed = "de";
    let candidates = vec![
        "deploy".to_owned(),
        "describe".to_owned(),
        "detach".to_owned(),
    ];

    let mut menu = LineCompletion::open(typed, candidates, PROMPT_CHARS_MAX, CompletionCycle::Next)
        .expect("the host offers three candidates inside the bound");
    assert_eq!(menu.outcome(), CompletionOutcome::Listed);
    println!("typed: {PROMPT_PREFIX}{typed}");
    println!("line:  {PROMPT_PREFIX}{}", menu.selected());

    // The cycle wraps at both ends, so one key reaches every candidate.
    let mut seen = Vec::new();
    for _ in 0..menu.candidates().len() {
        seen.push(menu.selected().to_owned());
        menu.cycle(CompletionCycle::Next);
    }
    assert_eq!(seen, menu.candidates(), "the cycle reaches every candidate");
    assert_eq!(menu.selected(), "deploy", "the cycle wraps at the last row");

    // Every entry names the candidate alone. The prompt line shows the prefix,
    // so a row that repeated it would show the prefix twice.
    for entry in menu.candidates() {
        assert!(
            !entry.starts_with(PROMPT_PREFIX),
            "the entry {entry} carries no prompt prefix"
        );
    }

    menu.cycle(CompletionCycle::Next);
    let mut cells = CellBuffer::empty(BODY);
    draw_completion_menu(&mut cells, BODY, Theme::new(), &menu);
    println!("\nthe menu of kvim, drawn into cells that the host owns:");
    print!("{}", printable(&cells));

    // The bound refuses a longer list, so the host ranks and shortens its own
    // source instead of reading a menu that cut candidates in silence.
    let long: Vec<String> = (0..=COMPLETION_CANDIDATES_MAX)
        .map(|index| format!("candidate{index}"))
        .collect();
    assert!(
        LineCompletion::open(typed, long, PROMPT_CHARS_MAX, CompletionCycle::Next).is_none(),
        "the menu refuses more than {COMPLETION_CANDIDATES_MAX} candidates"
    );
    println!("a list above {COMPLETION_CANDIDATES_MAX} candidates opens no menu");

    // A cancel restores the text that the reader typed, so the prompt line
    // reads exactly as it did before the first completion key.
    let restored = menu.into_typed();
    assert_eq!(restored, typed);
    println!("after the cancel: {PROMPT_PREFIX}{restored}");
}

/// Returns the printable rows of one cell buffer.
fn printable(target: &CellBuffer) -> String {
    let mut out = String::new();
    for y in target.area.top()..target.area.bottom() {
        for x in target.area.left()..target.area.right() {
            out.push_str(target.cell((x, y)).map_or(" ", |cell| cell.symbol()));
        }
        out.push('\n');
    }
    out
}

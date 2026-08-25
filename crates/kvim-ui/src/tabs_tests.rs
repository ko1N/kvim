//! Unit tests for the bounded strip of named surfaces.

use super::*;

/// The surfaces that one host of these tests owns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Surface {
    Chat,
    Editor,
    Review,
}

fn strip() -> TabStrip<Surface> {
    let mut tabs = TabStrip::default();
    tabs.open(Surface::Chat, "Chat")
        .expect("the label is short");
    tabs.open(Surface::Editor, "Editor")
        .expect("the label is short");
    tabs.open(Surface::Review, "Review")
        .expect("the label is short");
    tabs
}

fn labels(tabs: &TabStrip<Surface>) -> Vec<&str> {
    tabs.tabs().map(|tab| tab.label).collect()
}

#[test]
fn the_first_tab_of_one_strip_owns_it() {
    let mut tabs = TabStrip::default();
    assert!(tabs.is_empty());
    assert_eq!(tabs.active(), None);

    tabs.open(Surface::Chat, "Chat")
        .expect("the label is short");
    assert_eq!(tabs.active(), Some(&Surface::Chat));
    assert_eq!(tabs.len(), 1);

    // A later tab opens behind it and changes no active tab.
    tabs.open(Surface::Editor, "Editor")
        .expect("the label is short");
    assert_eq!(tabs.active(), Some(&Surface::Chat));
    assert_eq!(labels(&tabs), vec!["Chat", "Editor"]);
}

#[test]
fn one_identity_opens_one_tab_and_renames_it() {
    let mut tabs = strip();
    tabs.open(Surface::Editor, "src/main.rs")
        .expect("the label is short");

    assert_eq!(tabs.len(), 3, "the identity opened no second tab");
    assert_eq!(labels(&tabs), vec!["Chat", "src/main.rs", "Review"]);
}

#[test]
fn the_walk_cycles_in_both_directions() {
    let mut tabs = strip();

    assert!(tabs.select_next());
    assert_eq!(tabs.active(), Some(&Surface::Editor));
    assert!(tabs.select_next());
    assert_eq!(tabs.active(), Some(&Surface::Review));
    // The last tab reaches the first one.
    assert!(tabs.select_next());
    assert_eq!(tabs.active(), Some(&Surface::Chat));
    // And the first reaches the last.
    assert!(tabs.select_previous());
    assert_eq!(tabs.active(), Some(&Surface::Review));
}

#[test]
fn a_strip_of_one_tab_walks_to_nothing_new() {
    let mut tabs = TabStrip::default();
    tabs.open(Surface::Chat, "Chat")
        .expect("the label is short");

    assert!(!tabs.select_next());
    assert!(!tabs.select_previous());
    assert_eq!(tabs.active(), Some(&Surface::Chat));
}

#[test]
fn a_selection_names_one_tab_and_refuses_every_other_identity() {
    let mut tabs = strip();
    assert!(tabs.select(&Surface::Review));
    assert_eq!(tabs.active(), Some(&Surface::Review));

    let mut without = TabStrip::default();
    without
        .open(Surface::Chat, "Chat")
        .expect("the label is short");
    assert!(!without.select(&Surface::Editor));
    assert_eq!(without.active(), Some(&Surface::Chat));
}

#[test]
fn a_close_keeps_one_active_tab_while_the_strip_holds_any() {
    let mut tabs = strip();
    tabs.select(&Surface::Review);

    // Closing the last tab falls back to the tab that becomes the last one.
    assert!(tabs.close(&Surface::Review));
    assert_eq!(tabs.active(), Some(&Surface::Editor));

    // Closing a tab that follows the active one keeps the active tab.
    tabs.select(&Surface::Chat);
    tabs.open(Surface::Review, "Review")
        .expect("the label is short");
    assert!(tabs.close(&Surface::Review));
    assert_eq!(tabs.active(), Some(&Surface::Chat));

    // Closing a tab before the active one keeps that tab active.
    tabs.select(&Surface::Editor);
    assert!(tabs.close(&Surface::Chat));
    assert_eq!(tabs.active(), Some(&Surface::Editor));

    // The last tab leaves the strip empty.
    assert!(tabs.close(&Surface::Editor));
    assert!(tabs.is_empty());
    assert_eq!(tabs.active(), None);

    // A close of an identity that the strip never held changes nothing.
    assert!(!tabs.close(&Surface::Chat));
}

#[test]
fn a_strip_holds_its_bound_and_refuses_one_more() {
    let mut tabs: TabStrip<usize> = TabStrip::default();
    for index in 0..TABS_MAX {
        tabs.open(index, "tab").expect("the bound holds this tab");
    }
    assert_eq!(tabs.len(), TABS_MAX);

    assert_eq!(
        tabs.open(TABS_MAX, "one more"),
        Err(TabError::Limit { max: TABS_MAX })
    );
}

#[test]
fn a_label_stays_inside_its_bound() {
    let mut tabs: TabStrip<usize> = TabStrip::default();

    assert_eq!(
        tabs.open(0, ""),
        Err(TabError::Label {
            actual: 0,
            max: TAB_LABEL_CHARS_MAX,
        })
    );
    let long = "x".repeat(TAB_LABEL_CHARS_MAX + 1);
    assert_eq!(
        tabs.open(0, &long),
        Err(TabError::Label {
            actual: TAB_LABEL_CHARS_MAX + 1,
            max: TAB_LABEL_CHARS_MAX,
        })
    );
    assert!(tabs.is_empty(), "a refused tab reaches no strip");
}

#[test]
fn a_placement_names_every_tab_that_fits_whole() {
    let tabs = strip();
    // "Chat" takes six cells with its padding, "Editor" eight, "Review" eight.
    let wide = tabs.placements(Rect::new(0, 0, 40, 1));
    assert_eq!(wide.len(), 3);
    assert_eq!(wide[0].area, Rect::new(0, 0, 6, 1));
    assert_eq!(wide[1].area, Rect::new(6, 0, 8, 1));
    assert_eq!(wide[2].area, Rect::new(14, 0, 8, 1));
    assert!(wide[0].tab.active);
    assert!(!wide[1].tab.active);

    // A rectangle that cannot hold the third tab whole names two.
    let narrow = tabs.placements(Rect::new(0, 0, 20, 1));
    assert_eq!(narrow.len(), 2);

    // A rectangle without cells names none.
    assert!(tabs.placements(Rect::new(0, 0, 0, 1)).is_empty());
}

#[test]
fn the_render_callback_draws_every_visible_tab() {
    use ratatui::style::Style;

    let tabs = strip();
    let area = Rect::new(0, 0, 40, 1);
    let mut target = Buffer::empty(area);

    tabs.render(&mut target, area, |buffer, placement| {
        buffer.set_string(
            placement.area.x,
            placement.area.y,
            format!(" {} ", placement.tab.label),
            Style::default(),
        );
    });

    let drawn: String = (0..area.width)
        .map(|x| target[(x, 0)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(drawn.starts_with(" Chat  Editor  Review "), "{drawn:?}");
}

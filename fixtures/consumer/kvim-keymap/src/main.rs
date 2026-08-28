use kvim_keymap::{Key, KeyCode, KeySequence};

fn main() {
    let keys = [
        Key::plain(KeyCode::Char('g')),
        Key::plain(KeyCode::Char('d')),
    ];
    let sequence = KeySequence::new(&keys, 4).expect("two keys fit the host limit");
    assert_eq!(sequence.keys(), keys.as_slice());
    assert_eq!(sequence.to_string(), "g d");
}

use std::fmt;

use super::{BINDINGS_MAX, Registry, RegistryError};
use crate::binding::{Binding, CommandMetadata, CommandOwner, SCOPES_MAX, Scope};
use crate::key::{Key, KeyCode};
use crate::sequence::SEQUENCE_KEYS_MAX;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Action {
    First,
    Second,
    LongId,
    LongLabel,
}

impl fmt::Display for Action {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

impl CommandMetadata for Action {
    fn id(&self) -> &str {
        match self {
            Self::First => "first",
            Self::Second => "second",
            Self::LongId => LONG_TEXT,
            Self::LongLabel => "long-label",
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::First => "First",
            Self::Second => "Second",
            Self::LongId => "Long identifier",
            Self::LongLabel => LONG_TEXT,
        }
    }
}

/// Text that breaks both metadata bounds.
const LONG_TEXT: &str = "0123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Table {
    Global,
    Overlay,
}

impl fmt::Display for Table {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Global => "Global",
            Self::Overlay => "Overlay",
        })
    }
}

impl Scope for Table {
    const COUNT: usize = 2;
}

/// A scope type that declares more tables than a registry accepts.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WideTable;

impl fmt::Display for WideTable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Wide")
    }
}

impl Scope for WideTable {
    const COUNT: usize = SCOPES_MAX + 1;
}

/// A scope type that declares fewer tables than its values need.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum NarrowTable {
    First,
    Second,
}

impl fmt::Display for NarrowTable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::First => "First",
            Self::Second => "Second",
        })
    }
}

impl Scope for NarrowTable {
    const COUNT: usize = 1;
}

fn ch(value: char) -> Key {
    Key::plain(KeyCode::Char(value))
}

fn registry(
    bindings: &[Binding<Action, Table>],
) -> Result<Registry<Action, Table>, RegistryError<Action, Table>> {
    Registry::from_bindings(bindings, 4)
}

#[test]
fn one_sequence_reaches_one_command_in_each_scope() {
    let keys = [ch('g'), ch('d')];
    let built = registry(&[
        Binding::surface(Table::Global, &keys, Action::First),
        Binding::host(Table::Overlay, &keys, Action::Second),
    ])
    .expect("two scopes may share one sequence");

    assert_eq!(built.command(Table::Global, &keys), Some(Action::First));
    assert_eq!(
        built
            .bound_command(Table::Overlay, &keys)
            .map(|bound| bound.owner),
        Some(CommandOwner::Host)
    );
    assert!(built.has_longer_sequence(Table::Global, &[ch('g')]));
    assert!(!built.has_longer_sequence(Table::Global, &keys));
    assert_eq!(built.len(), 2);
}

#[test]
fn a_duplicate_sequence_is_rejected() {
    let keys = [ch('d')];
    let error = registry(&[
        Binding::surface(Table::Global, &keys, Action::First),
        Binding::surface(Table::Global, &keys, Action::Second),
    ])
    .expect_err("one scope holds one command for one sequence");

    assert!(matches!(
        error,
        RegistryError::DuplicateSequence {
            scope: Table::Global,
            first: Action::First,
            second: Action::Second,
            ..
        }
    ));
}

#[test]
fn a_strict_prefix_pair_is_rejected() {
    let error = registry(&[
        Binding::surface(Table::Global, &[ch('g')], Action::First),
        Binding::surface(Table::Global, &[ch('g'), ch('d')], Action::Second),
    ])
    .expect_err("a bound prefix blocks the longer sequence");

    assert!(matches!(
        error,
        RegistryError::AmbiguousPrefix {
            prefix_command: Action::First,
            longer_command: Action::Second,
            ..
        }
    ));
}

#[test]
fn every_bound_returns_its_own_error() {
    let empty = registry(&[Binding::surface(Table::Global, &[], Action::First)])
        .expect_err("a binding holds at least one key");
    assert!(matches!(empty, RegistryError::EmptySequence { .. }));

    let long = registry(&[Binding::surface(
        Table::Global,
        &[ch('a'), ch('b'), ch('c'), ch('d'), ch('e')],
        Action::First,
    )])
    .expect_err("five keys break the limit of four");
    assert!(matches!(
        long,
        RegistryError::SequenceTooLong {
            keys: 5,
            keys_max: 4,
            ..
        }
    ));

    let limit = Registry::from_bindings(
        &[Binding::surface(Table::Global, &[ch('a')], Action::First)],
        SEQUENCE_KEYS_MAX + 1,
    )
    .expect_err("the limit stays inside the sequence ceiling");
    assert!(matches!(
        limit,
        RegistryError::SequenceLimitOutOfRange { .. }
    ));

    let scopes =
        Registry::from_bindings(&[Binding::surface(WideTable, &[ch('a')], Action::First)], 4)
            .expect_err("a scope type declares at most SCOPES_MAX tables");
    assert!(matches!(scopes, RegistryError::TooManyScopes { .. }));

    let identifier = registry(&[Binding::surface(Table::Global, &[ch('a')], Action::LongId)])
        .expect_err("a command identifier stays short");
    assert!(matches!(identifier, RegistryError::CommandIdTooLong { .. }));

    let label = registry(&[Binding::surface(
        Table::Global,
        &[ch('a')],
        Action::LongLabel,
    )])
    .expect_err("a command label stays short");
    assert!(matches!(label, RegistryError::CommandLabelTooLong { .. }));
}

#[test]
fn a_scope_type_that_under_declares_its_count_is_rejected() {
    let error = Registry::from_bindings(
        &[
            Binding::surface(NarrowTable::First, &[ch('a')], Action::First),
            Binding::surface(NarrowTable::Second, &[ch('a')], Action::Second),
        ],
        4,
    )
    .expect_err("the declared scope count must cover every used scope");

    assert!(matches!(
        error,
        RegistryError::UndeclaredScopes {
            scopes: 2,
            declared: 1
        }
    ));
}

#[test]
fn the_binding_count_stays_inside_its_bound() {
    let mut bindings = Vec::with_capacity(BINDINGS_MAX + 1);
    for index in 0..=BINDINGS_MAX {
        // The sequences repeat, because the count check runs first.
        bindings.push(Binding::surface(
            Table::Global,
            &[ch(char::from(
                b'a' + u8::try_from(index % 26).expect("the value is below 26"),
            ))],
            Action::First,
        ));
    }

    let error = registry(&bindings).expect_err("the registry rejects the burst");

    assert!(matches!(error, RegistryError::TooManyBindings { .. }));
}

#[test]
fn prefix_extensions_keep_registry_order() {
    let built = registry(&[
        Binding::surface(Table::Global, &[ch('g'), ch('d')], Action::First),
        Binding::surface(Table::Global, &[ch('g'), ch('g')], Action::Second),
        Binding::surface(Table::Global, &[ch('z')], Action::First),
    ])
    .expect("no prefix pair collides");

    let reached: Vec<_> = built
        .extensions_of_prefix(Table::Global, &[ch('g')])
        .map(|(keys, _)| keys.to_string())
        .collect();

    assert_eq!(reached, vec!["g d".to_owned(), "g g".to_owned()]);
}

#[test]
fn one_hint_names_every_distinct_command_behind_its_next_key() {
    let built = registry(&[
        // `g d` and `g g` reach one command each, and `f a` and `f b` both
        // reach `First`, so that group counts one command.
        Binding::surface(Table::Global, &[ch('f'), ch('a')], Action::First),
        Binding::surface(Table::Global, &[ch('f'), ch('b')], Action::First),
        Binding::surface(Table::Global, &[ch('g'), ch('d')], Action::First),
        Binding::surface(Table::Global, &[ch('g'), ch('g')], Action::Second),
    ])
    .expect("no prefix pair collides");

    let hints = built.hints_for_prefix(Table::Global, &[]);
    let reached: Vec<_> = hints
        .iter()
        .map(|hint| (hint.key_label().to_string(), hint.target().to_string()))
        .collect();

    assert_eq!(
        reached,
        vec![
            ("f".to_owned(), "First".to_owned()),
            ("g".to_owned(), "+2 commands".to_owned()),
        ],
        "the hints keep the key order of the registry"
    );
    assert_eq!(
        hints[0].commands(),
        [Action::First],
        "two sequences that reach one command count it once"
    );
    assert_eq!(hints[1].commands(), [Action::First, Action::Second]);
}

#[test]
fn all_bindings_matches_a_per_scope_walk_with_nothing_missing_or_repeated() {
    let built = registry(&[
        Binding::surface(Table::Global, &[ch('a')], Action::First),
        Binding::surface(Table::Global, &[ch('b')], Action::Second),
        Binding::host(Table::Overlay, &[ch('a')], Action::Second),
    ])
    .expect("two scopes may share one sequence");

    let walked: Vec<_> = built
        .all_bindings()
        .map(|(scope, keys, bound)| (scope, keys.to_string(), bound.command))
        .collect();

    let mut expected = Vec::new();
    for scope in [Table::Global, Table::Overlay] {
        expected.extend(
            built
                .bindings(scope)
                .map(|(keys, bound)| (scope, keys.to_string(), bound.command)),
        );
    }

    assert_eq!(
        walked, expected,
        "the combined walk holds exactly what a per-scope walk holds"
    );
    assert_eq!(walked.len(), built.len());
}

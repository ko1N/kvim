use kvim_input::{BindingProfile, Command, CommandAuthority};

fn main() {
    assert_eq!(Command::SaveBuffer.authority(), CommandAuthority::Workspace);

    let manifest = BindingProfile::Embedded
        .manifest()
        .expect("the built-in embedded profile is valid");
    let binding_count = manifest.entries().len();
    let has_insert_indent = manifest
        .entries()
        .iter()
        .any(|entry| entry.command() == Command::InsertIndent);
    println!("embedded bindings: {binding_count}; insert indentation: {has_insert_indent}");
}

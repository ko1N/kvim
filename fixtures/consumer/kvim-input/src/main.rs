use kvim_input::{Command, CommandAuthority};

fn main() {
    assert_eq!(Command::SaveBuffer.authority(), CommandAuthority::Workspace);
}

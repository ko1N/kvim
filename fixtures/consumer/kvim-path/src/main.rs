use kvim_path::WorktreeRelativePath;

fn main() {
    let path = WorktreeRelativePath::new("notes/todo.md").expect("the path is relative");
    assert_eq!(path.as_path().to_str(), Some("notes/todo.md"));
}

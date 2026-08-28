use kvim_lsp::{ManagerLimits, ProjectManager};

fn main() {
    let manager = ProjectManager::new(ManagerLimits::default());
    assert_eq!(manager.projects(), 0);
}
